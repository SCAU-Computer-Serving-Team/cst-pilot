use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::FutureExt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_agent::BoxFuture;
use pi_ai::{
    CacheRetention, Model, SimpleStreamOptions, StopReason, TranscriptContext, Usage,
    calculate_cost, now_ms,
};
use pi_session::{
    CacheWarmingAction, CacheWarmingDecision, CacheWarmingDecisionEvent, SessionEntry, UsageEntry,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::model_runtime::ModelRuntime;
use crate::settings_manager::CacheWarmingMode;

pub const MAX_WARMING_AGE_MS: u64 = 60 * 60_000;
pub const MAX_IDLE_WARMING_AGE_MS: u64 = 30 * 60_000;
pub const CACHE_WARMING_MINIMUM_EXPECTED_SAVINGS: f64 = 0.05;
pub const IDLE_CONTINUATION_PROBABILITY: f64 = 0.15;

pub fn get_cache_warming_delay_ms(ttl_ms: u64) -> Option<u64> {
    if ttl_ms <= 10_000 {
        return None;
    }
    let ninety_pct = ((ttl_ms as f64) * 0.9).floor() as u64;
    let margin_cap = ttl_ms.saturating_sub(10_000);
    Some(ninety_pct.min(margin_cap).max(1))
}

pub fn get_prompt_cache_ttl_ms(
    model: &Model,
    options: Option<&SimpleStreamOptions>,
) -> Option<u64> {
    let retention = options
        .and_then(|o| o.base.cache_retention)
        .unwrap_or_else(|| {
            if pi_ai::api::get_provider_env(
                "PI_CACHE_RETENTION",
                options.and_then(|o| o.base.env.as_ref()),
            )
            .as_deref()
                == Some("long")
            {
                CacheRetention::Long
            } else {
                CacheRetention::Short
            }
        });
    if retention == CacheRetention::None {
        return None;
    }
    let pc = model.prompt_cache.as_ref()?;
    let secs = match retention {
        CacheRetention::Short => pc.short?,
        CacheRetention::Long => pc.long?,
        CacheRetention::None => return None,
    };
    let millis = secs * 1000.0;
    (millis.is_finite() && millis >= 1.0 && millis < u64::MAX as f64).then_some(millis as u64)
}

pub fn is_replayable(model: &Model, options: Option<&SimpleStreamOptions>) -> bool {
    let has_reasoning = options.and_then(|o| o.reasoning).is_some();
    if !has_reasoning || model.api != "anthropic-messages" {
        return true;
    }
    model
        .compat
        .as_ref()
        .and_then(|c| c.force_adaptive_thinking)
        == Some(true)
}

fn last_prompt_tokens(entries: &[SessionEntry]) -> u64 {
    for entry in entries.iter().rev() {
        if let SessionEntry::Message(m) = entry
            && let pi_agent::AgentMessage::Assistant(a) = &m.message
        {
            return a.usage.input + a.usage.cache_read + a.usage.cache_write;
        }
    }
    0
}

fn price_tokens(model: &Model, input: u64, output: u64, cache_read: u64, cache_write: u64) -> f64 {
    let mut usage = Usage {
        input,
        output,
        cache_read,
        cache_write,
        total_tokens: input + output + cache_read + cache_write,
        ..Usage::default()
    };
    calculate_cost(&model.cost, &mut usage);
    usage.cost.total
}

/// 可注入时钟接口（支持测试使用虚拟时钟推进预热定时器，无需真实等待数分钟 TTL）。
pub trait RuntimeClock: Send + Sync {
    fn now_ms(&self) -> u64;
    fn sleep(&self, duration: Duration, cancel: CancellationToken) -> BoxFuture<'static, bool>;
}

#[derive(Default, Clone)]
pub struct SystemRuntimeClock;

impl RuntimeClock for SystemRuntimeClock {
    fn now_ms(&self) -> u64 {
        now_ms()
    }

    fn sleep(&self, duration: Duration, cancel: CancellationToken) -> BoxFuture<'static, bool> {
        Box::pin(async move {
            tokio::select! {
                () = tokio::time::sleep(duration) => true,
                () = cancel.cancelled() => false,
            }
        })
    }
}

/// 供确定性单元测试使用的手动推进虚拟时钟。
#[derive(Clone)]
pub struct ManualTestClock {
    current_ms: Arc<AtomicU64>,
    notify: Arc<Notify>,
}

impl Default for ManualTestClock {
    fn default() -> Self {
        Self::new(1_700_000_000_000)
    }
}

impl ManualTestClock {
    pub fn new(initial_ms: u64) -> Self {
        Self {
            current_ms: Arc::new(AtomicU64::new(initial_ms)),
            notify: Arc::new(Notify::new()),
        }
    }

    pub fn set_ms(&self, ms: u64) {
        self.current_ms.store(ms, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn advance_ms(&self, delta_ms: u64) {
        self.current_ms.fetch_add(delta_ms, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
}

impl RuntimeClock for ManualTestClock {
    fn now_ms(&self) -> u64 {
        self.current_ms.load(Ordering::SeqCst)
    }

    fn sleep(&self, duration: Duration, cancel: CancellationToken) -> BoxFuture<'static, bool> {
        let target_ms = self.now_ms().saturating_add(duration.as_millis() as u64);
        let current = self.current_ms.clone();
        let notify = self.notify.clone();
        Box::pin(async move {
            loop {
                if cancel.is_cancelled() {
                    return false;
                }
                if current.load(Ordering::SeqCst) >= target_ms {
                    return true;
                }
                let notified = notify.notified();
                if current.load(Ordering::SeqCst) >= target_ms {
                    return true;
                }
                tokio::select! {
                    () = notified => {}
                    () = cancel.cancelled() => return false,
                }
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheWarmingStatus {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_warm_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<CacheWarmingDecision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_override: Option<bool>,
}

#[derive(Clone)]
pub struct CacheWarmRequest {
    pub session_id: String,
    pub session_generation: u64,
    pub selection_generation: u64,
    pub request_seq: u64,
    pub selected_model: Model,
    pub physical_model: Model,
    pub transcript: TranscriptContext,
    pub options: SimpleStreamOptions,
}

#[derive(Clone)]
struct ActiveWarmRun {
    run_id: u64,
    request: CacheWarmRequest,
    is_current: Arc<dyn Fn() -> bool + Send + Sync>,
    ttl_ms: u64,
    delay_ms: u64,
    refresh_deadline_at: u64,
    started_at: u64,
    cancel: CancellationToken,
    phase: String, // "streaming" | "idle"
    next_warm_at: u64,
    extension_override: bool,
    refreshing: bool,
}

pub type DecideWarmingFn =
    Arc<dyn Fn(CacheWarmingDecisionEvent) -> BoxFuture<'static, CacheWarmingAction> + Send + Sync>;
pub type CurrentWarmContext = Arc<dyn Fn() -> bool + Send + Sync>;
pub type AppendWarmUsageFn = Arc<
    dyn Fn(
            String,
            String,
            String,
            Usage,
            Option<String>,
            CurrentWarmContext,
        ) -> Result<Option<UsageEntry>, String>
        + Send
        + Sync,
>;

type WarmedCallback = Arc<dyn Fn(UsageEntry) + Send + Sync>;

struct CacheWarmerInner {
    active_run: Option<ActiveWarmRun>,
    inactive_status: CacheWarmingStatus,
    next_run_id: u64,
    latest_request: Option<(String, u64, u64)>,
}

/// Prompt 缓存预热器（对照 `CacheWarmer`）。
#[derive(Clone)]
pub struct CacheWarmer {
    inner: Arc<Mutex<CacheWarmerInner>>,
    model_runtime: ModelRuntime,
    get_branch: Arc<dyn Fn() -> Vec<SessionEntry> + Send + Sync>,
    append_usage: AppendWarmUsageFn,
    on_storage_error: Arc<dyn Fn(String) + Send + Sync>,
    get_mode: Arc<dyn Fn() -> CacheWarmingMode + Send + Sync>,
    decide: DecideWarmingFn,
    clock: Arc<dyn RuntimeClock>,
    on_warmed: Arc<Mutex<Option<WarmedCallback>>>,
}

impl CacheWarmer {
    pub fn new(
        model_runtime: ModelRuntime,
        get_branch: Arc<dyn Fn() -> Vec<SessionEntry> + Send + Sync>,
        append_usage: AppendWarmUsageFn,
        on_storage_error: Arc<dyn Fn(String) + Send + Sync>,
        get_mode: Arc<dyn Fn() -> CacheWarmingMode + Send + Sync>,
        decide: DecideWarmingFn,
        clock: Option<Arc<dyn RuntimeClock>>,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CacheWarmerInner {
                active_run: None,
                inactive_status: CacheWarmingStatus {
                    state: "inactive".to_owned(),
                    reason: Some("waiting for first request".to_owned()),
                    next_warm_at: None,
                    decision: None,
                    extension_override: None,
                },
                next_run_id: 1,
                latest_request: None,
            })),
            model_runtime,
            get_branch,
            append_usage,
            on_storage_error,
            get_mode,
            decide,
            clock: clock.unwrap_or_else(|| Arc::new(SystemRuntimeClock)),
            on_warmed: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_on_warmed(&self, cb: Option<Arc<dyn Fn(UsageEntry) + Send + Sync>>) {
        *self.on_warmed.lock().expect("on_warmed lock") = cb;
    }

    pub fn status(&self) -> CacheWarmingStatus {
        if (self.get_mode)() == CacheWarmingMode::Off {
            return CacheWarmingStatus {
                state: "inactive".to_owned(),
                reason: Some("cache warming disabled".to_owned()),
                next_warm_at: None,
                decision: None,
                extension_override: None,
            };
        }
        let (active, inactive) = {
            let guard = self.inner.lock().expect("warmer lock");
            (guard.active_run.clone(), guard.inactive_status.clone())
        };
        let Some(run) = active else {
            return inactive;
        };
        if !(run.is_current)() {
            return CacheWarmingStatus {
                state: "inactive".to_owned(),
                reason: Some("conversation context changed".to_owned()),
                next_warm_at: None,
                decision: None,
                extension_override: None,
            };
        }
        let decision = self.evaluate_run(&run);
        if !decision.economics_available && !run.refreshing {
            return CacheWarmingStatus {
                state: "inactive".to_owned(),
                reason: Some("cache economics unavailable".to_owned()),
                next_warm_at: None,
                decision: None,
                extension_override: None,
            };
        }
        CacheWarmingStatus {
            state: if run.refreshing {
                "refreshing".to_owned()
            } else {
                "scheduled".to_owned()
            },
            reason: None,
            next_warm_at: Some(run.next_warm_at),
            decision: Some(decision),
            extension_override: Some(run.extension_override),
        }
    }

    pub fn start(
        &self,
        request: CacheWarmRequest,
        is_current: Arc<dyn Fn() -> bool + Send + Sync>,
    ) {
        let mode = (self.get_mode)();
        let ttl_ms = get_prompt_cache_ttl_ms(&request.physical_model, Some(&request.options));
        let timing =
            ttl_ms.and_then(|ttl| get_cache_warming_delay_ms(ttl).map(|delay| (ttl, delay)));
        let stop_reason = if mode == CacheWarmingMode::Off {
            Some("cache warming disabled")
        } else if !is_replayable(&request.physical_model, Some(&request.options)) {
            Some("request cannot be replayed safely")
        } else if request.options.base.cache_retention == Some(CacheRetention::None) {
            Some("request disabled prompt caching")
        } else if timing.is_none() {
            Some("cache lifetime unavailable")
        } else {
            None
        };

        let now = self.clock.now_ms();
        let mut guard = self.inner.lock().expect("warmer lock");
        if guard
            .latest_request
            .as_ref()
            .is_some_and(|(id, generation, seq)| {
                *id == request.session_id
                    && (*generation, *seq) >= (request.session_generation, request.request_seq)
            })
        {
            return;
        }
        guard.latest_request = Some((
            request.session_id.clone(),
            request.session_generation,
            request.request_seq,
        ));
        if let Some(previous) = guard.active_run.take() {
            previous.cancel.cancel();
        }
        if let Some(reason) = stop_reason {
            guard.inactive_status = CacheWarmingStatus {
                state: "inactive".into(),
                reason: Some(reason.into()),
                next_warm_at: None,
                decision: None,
                extension_override: None,
            };
            return;
        }
        let (ttl_ms, delay_ms) = timing.expect("validated timing");
        let run_id = guard.next_run_id;
        guard.next_run_id += 1;

        let run = ActiveWarmRun {
            run_id,
            request,
            is_current,
            ttl_ms,
            delay_ms,
            refresh_deadline_at: 0,
            started_at: now,
            cancel: CancellationToken::new(),
            phase: "streaming".to_owned(),
            next_warm_at: 0,
            extension_override: false,
            refreshing: false,
        };
        guard.active_run = Some(run);
        drop(guard);

        self.schedule_run(run_id);
    }

    pub fn on_agent_settled(&self) {
        let mode = (self.get_mode)();
        let now = self.clock.now_ms();
        let mut guard = self.inner.lock().expect("warmer lock");
        let Some(ref mut run) = guard.active_run else {
            return;
        };
        if mode == CacheWarmingMode::Streaming {
            let id = run.run_id;
            drop(guard);
            self.stop_run(id, "agent run settled", None, None);
            return;
        }
        run.phase = "idle".to_owned();
        let deadline = run.started_at.saturating_add(MAX_IDLE_WARMING_AGE_MS);
        if run.next_warm_at > deadline || now >= deadline {
            let id = run.run_id;
            drop(guard);
            self.stop_run(id, "30-minute idle safety limit reached", None, None);
        }
    }

    pub fn on_mode_changed(&self) {
        let active = self.inner.lock().expect("warmer lock").active_run.clone();
        if let Some(run) = active
            && let Some(reason) = self.mode_stop_reason(&run)
        {
            self.stop_run(run.run_id, &reason, None, None);
        }
    }

    pub fn cancel(&self) {
        self.stop_with("inactive", None, None);
    }

    fn stop_with(
        &self,
        reason: &str,
        decision: Option<CacheWarmingDecision>,
        extension_override: Option<bool>,
    ) {
        let mut guard = self.inner.lock().expect("warmer lock");
        if let Some(prev) = guard.active_run.take() {
            prev.cancel.cancel();
        }
        guard.inactive_status = CacheWarmingStatus {
            state: "inactive".to_owned(),
            reason: Some(reason.to_owned()),
            next_warm_at: None,
            decision,
            extension_override,
        };
    }

    fn schedule_run(&self, run_id: u64) {
        let now = self.clock.now_ms();
        let (delay_ms, cancel) = {
            let mut guard = self.inner.lock().expect("warmer lock");
            let Some(ref mut run) = guard.active_run else {
                return;
            };
            if run.run_id != run_id {
                return;
            }
            run.extension_override = false;
            run.refreshing = false;
            run.next_warm_at = now.saturating_add(run.delay_ms);
            run.refresh_deadline_at = run
                .next_warm_at
                .saturating_add((run.ttl_ms.saturating_sub(run.delay_ms)) / 2);

            let max_window = if run.phase == "idle" {
                MAX_IDLE_WARMING_AGE_MS
            } else {
                MAX_WARMING_AGE_MS
            };
            let deadline = run.started_at.saturating_add(max_window);
            if run.next_warm_at > deadline || now >= deadline {
                let reason = if run.phase == "idle" {
                    "30-minute idle safety limit reached"
                } else {
                    "one-hour safety limit reached"
                };
                drop(guard);
                self.stop_run(run_id, reason, None, None);
                return;
            }
            (run.delay_ms, run.cancel.clone())
        };

        let warmer = self.clone();
        // Construct the timer now: a spawned task may first poll after a clock advance.
        let sleep = self.clock.sleep(Duration::from_millis(delay_ms), cancel);
        tokio::spawn(async move {
            let woke = sleep.await;
            if woke {
                warmer.refresh_run(run_id).await;
            }
        });
    }

    async fn refresh_run(&self, run_id: u64) {
        let run_snapshot = {
            let mut guard = self.inner.lock().expect("warmer lock");
            let Some(ref mut run) = guard.active_run else {
                return;
            };
            if run.run_id != run_id {
                return;
            }
            run.refreshing = true;
            run.clone()
        };

        if !self.validate_run(run_id) || self.refresh_deadline_missed(&run_snapshot) {
            return;
        }

        let decision = self.evaluate_run(&run_snapshot);
        let event = CacheWarmingDecisionEvent {
            warm_cost: decision.warm_cost,
            miss_cost: decision.miss_cost,
            continuation_probability: decision.continuation_probability,
            action: decision.action,
        };
        let decision_future = std::panic::catch_unwind(AssertUnwindSafe(|| (self.decide)(event)));
        let action = match decision_future {
            Ok(future) => tokio::select! { biased;
                _ = run_snapshot.cancel.cancelled() => return,
                result = AssertUnwindSafe(future).catch_unwind() => result.unwrap_or(decision.action),
            },
            Err(_) => decision.action,
        };

        if !self.validate_run(run_id) || self.refresh_deadline_missed(&run_snapshot) {
            return;
        }

        let ext_override = action != decision.action;
        if action == CacheWarmingAction::Stop {
            let reason = if ext_override {
                "stopped by extension"
            } else if decision.economics_available {
                "expected savings below threshold"
            } else {
                "cache economics unavailable"
            };
            self.stop_run(run_id, reason, Some(decision), Some(ext_override));
            return;
        }

        {
            let mut guard = self.inner.lock().expect("warmer lock");
            if let Some(ref mut run) = guard.active_run
                && run.run_id == run_id
            {
                run.extension_override = ext_override;
            } else {
                return;
            }
        }

        let mut warm_opts = run_snapshot.request.options.clone();
        warm_opts.base.max_tokens = Some(1);
        warm_opts.base.max_retries = Some(0);
        warm_opts.base.cancel_token = Some(run_snapshot.cancel.clone());
        // 使用独立路由 ID 以避免重入触发 CacheWarmer::start
        warm_opts.base.session_id = Some(format!(
            "{}#cache_warm_{}",
            run_snapshot.request.session_id, run_id
        ));

        let replay = async {
            let stream = self
                .model_runtime
                .stream_simple(
                    run_snapshot.request.physical_model.clone(),
                    run_snapshot.request.transcript.clone(),
                    warm_opts,
                )
                .await;
            stream.discard_queued_events();
            stream.result().await
        };
        let message = tokio::select! { biased;
            _ = run_snapshot.cancel.cancelled() => return,
            message = replay => message,
        };

        if !self.validate_run(run_id) {
            return;
        }

        if !matches!(message.stop_reason, StopReason::Error | StopReason::Aborted) {
            let note = ext_override.then(|| "extension override".to_owned());
            match (self.append_usage)(
                "cache_warm".to_owned(),
                message.provider.clone(),
                message
                    .response_model
                    .clone()
                    .unwrap_or_else(|| message.model.clone()),
                message.usage.clone(),
                note,
                run_snapshot.is_current.clone(),
            ) {
                Ok(Some(entry)) => {
                    let callback = self.on_warmed.lock().expect("on_warmed lock").clone();
                    if let Some(cb) = callback {
                        cb(entry);
                    }
                }
                Ok(None) => {
                    self.stop_run(run_id, "context changed before usage commit", None, None);
                    return;
                }
                Err(err) => {
                    self.stop_run(run_id, "storage error during cache warming", None, None);
                    (self.on_storage_error)(format!(
                        "Failed to persist cache warming usage: {err}"
                    ));
                    return;
                }
            }
        }

        let still_active = self
            .inner
            .lock()
            .expect("warmer lock")
            .active_run
            .as_ref()
            .is_some_and(|r| r.run_id == run_id);
        if still_active {
            self.schedule_run(run_id);
        }
    }

    fn refresh_deadline_missed(&self, run: &ActiveWarmRun) -> bool {
        if self.clock.now_ms() <= run.refresh_deadline_at {
            return false;
        }
        self.stop_run(run.run_id, "cache refresh deadline missed", None, None);
        true
    }

    fn stop_run(
        &self,
        run_id: u64,
        reason: &str,
        decision: Option<CacheWarmingDecision>,
        extension_override: Option<bool>,
    ) {
        let mut guard = self.inner.lock().expect("warmer lock");
        if !guard
            .active_run
            .as_ref()
            .is_some_and(|run| run.run_id == run_id)
        {
            return;
        }
        if let Some(run) = guard.active_run.take() {
            run.cancel.cancel();
        }
        guard.inactive_status = CacheWarmingStatus {
            state: "inactive".into(),
            reason: Some(reason.into()),
            next_warm_at: None,
            decision,
            extension_override,
        };
    }

    fn validate_run(&self, run_id: u64) -> bool {
        let active = self.inner.lock().expect("warmer lock").active_run.clone();
        let Some(run) = active.filter(|run| run.run_id == run_id) else {
            return false;
        };
        let max_window = if run.phase == "idle" {
            MAX_IDLE_WARMING_AGE_MS
        } else {
            MAX_WARMING_AGE_MS
        };
        let stop_reason = self.mode_stop_reason(&run).or_else(|| {
            if self.clock.now_ms() > run.started_at.saturating_add(max_window) {
                Some(
                    if run.phase == "idle" {
                        "30-minute idle safety limit reached"
                    } else {
                        "one-hour safety limit reached"
                    }
                    .into(),
                )
            } else if !(run.is_current)() {
                Some("conversation context changed".into())
            } else {
                None
            }
        });
        if let Some(reason) = stop_reason {
            self.stop_run(run_id, &reason, None, None);
            return false;
        }
        !run.cancel.is_cancelled()
    }

    fn mode_stop_reason(&self, run: &ActiveWarmRun) -> Option<String> {
        let mode = (self.get_mode)();
        if mode == CacheWarmingMode::Off {
            return Some("cache warming disabled".to_owned());
        }
        if mode == CacheWarmingMode::Streaming && run.phase == "idle" {
            return Some("agent run settled".to_owned());
        }
        None
    }

    fn evaluate_run(&self, run: &ActiveWarmRun) -> CacheWarmingDecision {
        let model = &run.request.physical_model;
        let branch = (self.get_branch)();
        let prompt_tokens = last_prompt_tokens(&branch);
        let cache_hit_cost = price_tokens(model, 0, 0, prompt_tokens, 0);
        let cache_miss_cost = if model.cost.cache_write > 0.0 {
            price_tokens(model, 0, 0, 0, prompt_tokens)
        } else {
            price_tokens(model, prompt_tokens, 0, 0, 0)
        };
        let warm_cost = price_tokens(model, 0, 1, prompt_tokens, 0);
        let miss_cost = (cache_miss_cost - cache_hit_cost).max(0.0);
        let continuation_probability = if run.phase == "idle" {
            IDLE_CONTINUATION_PROBABILITY
        } else {
            1.0
        };
        let economics_available = prompt_tokens > 0
            && [cache_hit_cost, cache_miss_cost, warm_cost, miss_cost]
                .iter()
                .all(|cost| cost.is_finite() && *cost >= 0.0)
            && (cache_hit_cost > 0.0 || cache_miss_cost > 0.0);
        let expected_savings = continuation_probability * miss_cost - warm_cost;
        let action =
            if economics_available && expected_savings >= CACHE_WARMING_MINIMUM_EXPECTED_SAVINGS {
                CacheWarmingAction::Warm
            } else {
                CacheWarmingAction::Stop
            };

        CacheWarmingDecision {
            phase: run.phase.clone(),
            warm_cost,
            miss_cost,
            continuation_probability,
            expected_savings,
            economics_available,
            action,
        }
    }
}

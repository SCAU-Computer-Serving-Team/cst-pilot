use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use futures_util::{FutureExt, future::Shared};
use pi_ai::BoxFuture;
use serde_json::Value;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{
    billing::BillingRequest,
    collect::Collector,
    delivery::blocking,
    diagnostics::{Event, SnapshotRef},
    outbox::QueuedRecord,
    record::SessionRecord,
    service::{Backend, TelemetryService},
};

pub type SubmissionTask = Shared<BoxFuture<'static, Result<(), String>>>;
struct Frozen {
    record: SessionRecord,
    requests: Vec<BillingRequest>,
    last_activity: u64,
    sealed: bool,
}
struct State {
    collector: Collector,
    revision: u64,
    closing: bool,
    closed: bool,
    final_record: Option<SessionRecord>,
    sealed: bool,
    submitted: Option<Value>,
    tail: SubmissionTask,
    finalizing: Option<SubmissionTask>,
}
/// Freeze on the event thread; serialize persistence without waiting on model execution.
pub struct SessionSubmission {
    pub record_id: String,
    service: Weak<TelemetryService>,
    state: Mutex<State>,
    billing_gate: tokio::sync::Mutex<()>,
    billing_running: std::sync::atomic::AtomicBool,
}
impl SessionSubmission {
    pub(crate) fn new(service: &Arc<TelemetryService>, collector: Collector) -> Arc<Self> {
        Arc::new(Self {
            record_id: collector.record_id.clone(),
            service: Arc::downgrade(service),
            state: Mutex::new(State {
                collector,
                revision: 0,
                closing: false,
                closed: false,
                final_record: None,
                sealed: false,
                submitted: None,
                tail: futures_util::future::ready(Ok(())).boxed().shared(),
                finalizing: None,
            }),
            billing_gate: tokio::sync::Mutex::new(()),
            billing_running: false.into(),
        })
    }
    pub fn update(&self, f: impl FnOnce(&mut Collector)) {
        let mut state = self.state.lock().expect("submission");
        if !state.closing {
            f(&mut state.collector);
        }
    }
    fn freeze(state: &mut State, reason: &str, entries: Option<u64>, now: u64) -> Frozen {
        state.revision += 1;
        Frozen {
            record: state.collector.snapshot(
                state.revision,
                !reason.is_empty(),
                reason,
                if reason == "crash" {
                    state.collector.last_activity
                } else {
                    now
                },
                entries,
            ),
            requests: state.collector.billing.snapshot(),
            last_activity: state.collector.last_activity,
            sealed: false,
        }
    }
    fn schedule(
        state: &mut State,
        operation: BoxFuture<'static, Result<(), String>>,
    ) -> SubmissionTask {
        let previous = state.tail.clone();
        let handle = tokio::spawn(async move {
            let _ = previous.await;
            operation.await
        });
        let task = async move { handle.await.map_err(|_| "遥测提交任务中断。".to_owned())? }
            .boxed()
            .shared();
        state.tail = task.clone();
        task
    }
    pub fn checkpoint(self: &Arc<Self>) -> SubmissionTask {
        self.snapshot(false)
    }
    pub fn submit(self: &Arc<Self>) -> SubmissionTask {
        self.snapshot(true)
    }
    fn snapshot(self: &Arc<Self>, queue: bool) -> SubmissionTask {
        let mut state = self.state.lock().expect("submission");
        if state.closing || !state.collector.has_activity() {
            return state.tail.clone();
        }
        let Some(service) = self.service.upgrade() else {
            return state.tail.clone();
        };
        let frozen = Self::freeze(&mut state, "", None, service.now());
        let this = self.clone();
        Self::schedule(
            &mut state,
            async move {
                let backend = service.backend().await?;
                let mut frozen = frozen;
                frozen.record.environment = backend.environment.clone();
                frozen.record.kit_version = backend.kit_version.clone();
                let fingerprint = fingerprint(&frozen.record)?;
                if queue
                    && this.state.lock().expect("submission").submitted.as_ref()
                        == Some(&fingerprint)
                {
                    return Ok(());
                }
                let record = this.persist(&service, &backend, frozen).await?;
                if queue {
                    backend
                        .delivery
                        .queue(record, &service.config.endpoints, CancellationToken::new())
                        .await?;
                    this.state.lock().expect("submission").submitted = Some(fingerprint);
                    service.kick();
                }
                Ok(())
            }
            .boxed(),
        )
    }
    async fn persist(
        &self,
        service: &TelemetryService,
        backend: &Backend,
        frozen: Frozen,
    ) -> Result<QueuedRecord, String> {
        let record = QueuedRecord::new(
            serde_json::to_value(&frozen.record).map_err(|_| "遥测快照无法编码。")?,
        )?;
        let drafts = backend.drafts.clone();
        let saved = record.clone();
        let result = blocking(CancellationToken::new(), move |token| {
            drafts.save_snapshot(
                &saved,
                frozen.last_activity,
                &frozen.requests,
                frozen.sealed,
                &token,
            )
        })
        .await;
        if result.is_err() {
            service.storage_failure(backend).await;
        }
        result?;
        backend.heartbeat.register(&self.record_id);
        backend
            .heartbeat
            .revision(&self.record_id, record.revision());
        backend
            .diagnostics
            .record_async(
                Event::Saved(SnapshotRef::from(&record)),
                service.now(),
                CancellationToken::new(),
            )
            .await;
        Ok(record)
    }
    pub fn settle_billing(self: &Arc<Self>) {
        use std::sync::atomic::Ordering;
        if self.billing_running.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            let _gate = this.billing_gate.lock().await;
            let Some(service) = this.service.upgrade() else {
                this.billing_running.store(false, Ordering::SeqCst);
                return;
            };
            let billing = {
                let state = this.state.lock().expect("submission");
                if state.closing {
                    this.billing_running.store(false, Ordering::SeqCst);
                    return;
                }
                state.collector.billing.clone()
            };
            let before = billing.summary();
            billing
                .settle(
                    service.lookup.as_ref(),
                    service.stop.child_token(),
                    Duration::from_millis(4500),
                )
                .await;
            drop(_gate);
            if before != billing.summary() {
                let _ = this.submit().await;
            }
            this.billing_running.store(false, Ordering::SeqCst);
        });
    }
    /// The deadline only bounds billing/waiting; durable local work continues after the caller leaves.
    pub fn finalize(
        self: &Arc<Self>,
        reason: &str,
        entries: Option<u64>,
        deadline: Instant,
    ) -> SubmissionTask {
        let mut state = self.state.lock().expect("submission");
        if state.closed {
            return state.tail.clone();
        }
        if let Some(task) = &state.finalizing {
            return task.clone();
        }
        let Some(service) = self.service.upgrade() else {
            return state.tail.clone();
        };
        state.closing = true;
        if state.final_record.is_none() {
            state.final_record =
                Some(Self::freeze(&mut state, reason, entries, service.now()).record);
        }
        let this = self.clone();
        let task = Self::schedule(
            &mut state,
            async move {
                let result = this.finish(service, deadline).await;
                if result.is_err() {
                    this.state.lock().expect("submission").finalizing = None;
                }
                result
            }
            .boxed(),
        );
        state.finalizing = Some(task.clone());
        task
    }
    async fn finish(
        &self,
        service: Arc<TelemetryService>,
        deadline: Instant,
    ) -> Result<(), String> {
        let backend = service.backend().await?;
        let (mut record, billing, last_activity, has_activity, sealed) = {
            let state = self.state.lock().expect("submission");
            (
                state.final_record.clone().expect("final snapshot"),
                state.collector.billing.clone(),
                state.collector.last_activity,
                state.collector.has_activity(),
                state.sealed,
            )
        };
        if !has_activity {
            service.finished(&self.record_id, &backend);
            self.state.lock().expect("submission").closed = true;
            return Ok(());
        }
        record.environment = backend.environment.clone();
        record.kit_version = backend.kit_version.clone();
        self.persist(
            &service,
            &backend,
            Frozen {
                record: record.clone(),
                requests: billing.snapshot(),
                last_activity,
                sealed,
            },
        )
        .await?;
        if !sealed
            && let Ok(_gate) = tokio::time::timeout_at(deadline, self.billing_gate.lock()).await
        {
            let cancel = CancellationToken::new();
            let _guard = cancel.clone().drop_guard();
            let settle = async {
                billing
                    .settle(
                        service.lookup.as_ref(),
                        cancel.clone(),
                        Duration::from_millis(4500),
                    )
                    .await;
                if billing
                    .summary()
                    .is_some_and(|s| s.requests != s.settled_requests)
                {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    billing
                        .settle(
                            service.lookup.as_ref(),
                            cancel.clone(),
                            Duration::from_millis(4500),
                        )
                        .await;
                }
            };
            let _ = tokio::time::timeout_at(
                deadline.min(Instant::now() + Duration::from_millis(4500)),
                settle,
            )
            .await;
            if record.cstoa_billing != billing.summary() {
                let mut state = self.state.lock().expect("submission");
                state.revision += 1;
                record.revision = state.revision;
                record.cstoa_billing = billing.summary();
            }
        }
        {
            let mut state = self.state.lock().expect("submission");
            state.final_record = Some(record.clone());
            state.sealed = true;
        }
        let queued = self
            .persist(
                &service,
                &backend,
                Frozen {
                    record,
                    requests: billing.snapshot(),
                    last_activity,
                    sealed: true,
                },
            )
            .await?;
        backend
            .delivery
            .queue(
                queued.clone(),
                &service.config.endpoints,
                CancellationToken::new(),
            )
            .await?;
        let drafts = backend.drafts.clone();
        blocking(CancellationToken::new(), move |token| {
            drafts.remove_if(&queued, &token)
        })
        .await?;
        service.finished(&self.record_id, &backend);
        self.state.lock().expect("submission").closed = true;
        service.kick();
        Ok(())
    }
    pub(crate) fn periodic(self: &Arc<Self>) -> SubmissionTask {
        let closing = self.state.lock().expect("submission").closing;
        if closing {
            self.finalize("crash", None, Instant::now() + Duration::from_millis(4500))
        } else {
            self.submit()
        }
    }
    pub async fn drain(&self) -> Result<(), String> {
        let task = self.state.lock().expect("submission").tail.clone();
        task.await
    }
}
fn fingerprint(record: &SessionRecord) -> Result<Value, String> {
    let mut value = serde_json::to_value(record).map_err(|_| "遥测快照无法编码。")?;
    let object = value.as_object_mut().ok_or("遥测快照格式无效。")?;
    for field in ["revision", "endedAt", "durationMs"] {
        object.remove(field);
    }
    Ok(value)
}

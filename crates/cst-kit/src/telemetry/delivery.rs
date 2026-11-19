use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use futures_util::future::join_all;
use tokio_util::sync::CancellationToken;

use super::{
    config::Endpoint,
    diagnostics::{Diagnostics, Event},
    outbox::{Outbox, QueuedRecord},
    transport::{FailureKind, SendOutcome, Sender, UploadAuth},
};

#[derive(Default)]
struct Flight {
    running: bool,
    rerun: bool,
}
struct Lease {
    state: Arc<Mutex<Flight>>,
    active: bool,
}
impl Lease {
    fn begin(state: Arc<Mutex<Flight>>) -> Option<Self> {
        {
            let mut state = state.lock().expect("delivery flight");
            if state.running {
                state.rerun = true;
                return None;
            }
            state.running = true;
            state.rerun = false;
        }
        Some(Self {
            state,
            active: true,
        })
    }
    fn again(&mut self) -> bool {
        let mut state = self.state.lock().expect("delivery flight");
        if state.rerun {
            state.rerun = false;
            true
        } else {
            state.running = false;
            self.active = false;
            false
        }
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if self.active {
            self.state.lock().expect("delivery flight").running = false;
        }
    }
}
#[derive(Clone)]
pub struct Delivery {
    outbox: Outbox,
    sender: Sender,
    diagnostics: Diagnostics,
    flights: Arc<Mutex<BTreeMap<String, Arc<Mutex<Flight>>>>>,
}
impl Delivery {
    pub fn new(outbox: Outbox, sender: Sender, diagnostics: Diagnostics) -> Self {
        Self {
            outbox,
            sender,
            diagnostics,
            flights: Arc::default(),
        }
    }
    pub async fn queue(
        &self,
        record: QueuedRecord,
        endpoints: &[Endpoint],
        cancel: CancellationToken,
    ) -> Result<(), String> {
        let mut failed = endpoints.is_empty();
        for endpoint in endpoints {
            let outbox = self.outbox.clone();
            let record = record.clone();
            let url = endpoint.url.clone();
            match blocking(cancel.clone(), move |token| {
                outbox.append(&record, Some(&url), &token)
            })
            .await
            {
                Ok(record) => {
                    self.diagnostics
                        .record_async(
                            Event::Queued {
                                endpoint: endpoint.url.clone(),
                                record: (&record).into(),
                            },
                            self.sender.now(),
                            cancel.clone(),
                        )
                        .await
                }
                Err(_) => failed = true,
            }
        }
        if failed {
            self.diagnostics
                .record_async(
                    Event::Failure {
                        endpoint: None,
                        kind: FailureKind::Storage,
                    },
                    self.sender.now(),
                    cancel,
                )
                .await;
            Err("遥测队列未全部保存，恢复草稿将保留。".into())
        } else {
            Ok(())
        }
    }
    pub async fn migrate(
        &self,
        endpoints: &[Endpoint],
        cancel: CancellationToken,
    ) -> Result<(), String> {
        let outbox = self.outbox.clone();
        let urls = endpoints.iter().map(|e| e.url.clone()).collect::<Vec<_>>();
        blocking(cancel, move |token| outbox.migrate_legacy(&urls, &token)).await
    }
    pub async fn flush(&self, auth: UploadAuth, endpoints: &[Endpoint], cancel: CancellationToken) {
        join_all(
            endpoints
                .iter()
                .map(|endpoint| self.flush_one(&auth, endpoint, cancel.clone())),
        )
        .await;
    }
    async fn failure(&self, endpoint: &str, kind: FailureKind, cancel: CancellationToken) {
        self.diagnostics
            .record_async(
                Event::Failure {
                    endpoint: Some(endpoint.into()),
                    kind,
                },
                self.sender.now(),
                cancel,
            )
            .await;
    }
    async fn flush_one(&self, auth: &UploadAuth, endpoint: &Endpoint, cancel: CancellationToken) {
        if endpoint.unavailable {
            self.failure(&endpoint.url, FailureKind::Tls, cancel).await;
            return;
        }
        let state = self
            .flights
            .lock()
            .expect("delivery flights")
            .entry(endpoint.url.clone())
            .or_default()
            .clone();
        let Some(mut lease) = Lease::begin(state) else {
            return;
        };
        loop {
            if self
                .flush_batches(auth, endpoint, cancel.clone())
                .await
                .is_err()
            {
                return;
            }
            if !lease.again() {
                return;
            }
        }
    }
    async fn flush_batches(
        &self,
        auth: &UploadAuth,
        endpoint: &Endpoint,
        cancel: CancellationToken,
    ) -> Result<(), ()> {
        loop {
            if cancel.is_cancelled() {
                return Err(());
            }
            let outbox = self.outbox.clone();
            let url = endpoint.url.clone();
            let batch = match blocking(cancel.clone(), move |_| outbox.batch(&url)).await {
                Ok(batch) => batch,
                Err(_) => {
                    self.failure(&endpoint.url, FailureKind::Storage, cancel)
                        .await;
                    return Err(());
                }
            };
            if batch.is_empty() {
                let outbox = self.outbox.clone();
                let url = endpoint.url.clone();
                match blocking(cancel.clone(), move |token| {
                    outbox.quarantine_head(&url, &token)
                })
                .await
                {
                    Ok(true) => {
                        self.failure(&endpoint.url, FailureKind::Protocol, cancel.clone())
                            .await;
                        continue;
                    }
                    Ok(false) => return Ok(()),
                    Err(_) => {
                        self.failure(&endpoint.url, FailureKind::Storage, cancel)
                            .await;
                        return Err(());
                    }
                }
            }
            let outcome = self
                .sender
                .send_with_fallback(endpoint, auth, &batch, None, cancel.clone())
                .await;
            let (remove, partial) = match &outcome {
                SendOutcome::Accepted {
                    acknowledged,
                    rejected,
                    ..
                } => {
                    let refs = acknowledged.clone();
                    let rejects = rejected.clone();
                    // Confirm disk removal before recording the diagnostic acknowledgment.
                    self.remove(&endpoint.url, refs.clone(), cancel.clone())
                        .await?;
                    self.diagnostics
                        .record_async(
                            Event::Acks {
                                endpoint: endpoint.url.clone(),
                                acknowledged: refs,
                                rejected: rejects,
                            },
                            self.sender.now(),
                            cancel.clone(),
                        )
                        .await;
                    (None, acknowledged.len() < batch.len())
                }
                SendOutcome::Bad => (Some(batch), false),
                _ => (None, false),
            };
            if let Some(records) = remove {
                self.remove(&endpoint.url, records, cancel.clone()).await?;
            }
            if partial {
                self.failure(&endpoint.url, FailureKind::Protocol, cancel)
                    .await;
                return Err(());
            }
            if let Some(failure) = outcome.failure() {
                self.failure(&endpoint.url, failure, cancel).await;
                return Err(());
            }
        }
    }
    async fn remove(
        &self,
        url: &str,
        records: Vec<QueuedRecord>,
        cancel: CancellationToken,
    ) -> Result<(), ()> {
        let outbox = self.outbox.clone();
        let endpoint = url.to_owned();
        if blocking(cancel.clone(), move |token| {
            outbox.acknowledge(&records, Some(&endpoint), &token)
        })
        .await
        .is_err()
        {
            self.failure(url, FailureKind::Storage, cancel).await;
            return Err(());
        }
        Ok(())
    }
}
/// Cancellation follows a dropped caller into blocking file work, without cancelling its parent.
pub(crate) async fn blocking<T: Send + 'static>(
    cancel: CancellationToken,
    operation: impl FnOnce(CancellationToken) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let child = cancel.child_token();
    let _guard = child.clone().drop_guard();
    tokio::task::spawn_blocking(move || operation(child))
        .await
        .map_err(|_| "遥测本地任务未完成。".to_owned())?
}

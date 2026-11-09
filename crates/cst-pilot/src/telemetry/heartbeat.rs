use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::{
    config::Endpoint,
    diagnostics::{Diagnostics, Event},
    environment::Identity,
    transport::{FailureKind, SendOutcome, Sender, UploadAuth, utc},
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Idle,
    Busy,
    Closed,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub record_id: String,
    pub revision: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Heartbeat {
    pub run_id: String,
    pub sequence: u64,
    pub sent_at: String,
    pub status: Status,
    pub identity: Identity,
    pub last_activity_at: Option<String>,
    pub snapshots: Vec<Snapshot>,
}
struct Instance {
    busy: bool,
    revision: Option<u64>,
}
struct Run {
    sequence: u64,
    generated_at: Option<u64>,
    last_activity: Option<u64>,
    closed: bool,
    instances: BTreeMap<String, Instance>,
}
struct Pending {
    heartbeat: Heartbeat,
    at: u64,
    auth: UploadAuth,
    endpoint: Endpoint,
}
#[derive(Default)]
struct Flight {
    running: bool,
    latest_sequence: u64,
    pending: Option<Pending>,
}
struct Lease {
    state: Arc<Mutex<Flight>>,
    active: bool,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if self.active {
            self.state.lock().expect("heartbeat flight").running = false;
        }
    }
}
#[derive(Clone)]
pub struct HeartbeatRun {
    pub run_id: String,
    identity: Identity,
    run: Arc<Mutex<Run>>,
    flights: Arc<Mutex<BTreeMap<String, Arc<Mutex<Flight>>>>>,
    sender: Sender,
    diagnostics: Diagnostics,
}
impl HeartbeatRun {
    pub fn new(identity: Identity, sender: Sender, diagnostics: Diagnostics) -> Self {
        Self {
            run_id: uuid::Uuid::new_v4().to_string(),
            identity,
            run: Arc::new(Mutex::new(Run {
                sequence: 0,
                generated_at: None,
                last_activity: None,
                closed: false,
                instances: BTreeMap::new(),
            })),
            flights: Arc::default(),
            sender,
            diagnostics,
        }
    }
    pub fn register(&self, record_id: &str) {
        let mut run = self.run.lock().expect("heartbeat run");
        if !run.closed && run.instances.len() < 512 {
            run.instances.entry(record_id.into()).or_insert(Instance {
                busy: false,
                revision: None,
            });
        }
    }
    pub fn revision(&self, record_id: &str, revision: u64) {
        if let Some(instance) = self
            .run
            .lock()
            .expect("heartbeat run")
            .instances
            .get_mut(record_id)
        {
            instance.revision = Some(revision);
        }
    }
    pub fn activity(&self, record_id: &str, busy: Option<bool>) {
        let mut run = self.run.lock().expect("heartbeat run");
        if run.closed {
            return;
        }
        if let Some(instance) = run.instances.get_mut(record_id) {
            if let Some(busy) = busy {
                instance.busy = busy;
            }
            run.last_activity = Some(self.sender.now());
        }
    }
    pub fn remove(&self, record_id: &str) {
        self.run
            .lock()
            .expect("heartbeat run")
            .instances
            .remove(record_id);
    }
    pub async fn pulse(
        &self,
        auth: UploadAuth,
        endpoints: &[Endpoint],
        close: bool,
        force: bool,
        cancel: CancellationToken,
    ) {
        let now = self.sender.now();
        let heartbeat = {
            let mut run = self.run.lock().expect("heartbeat run");
            if run.closed
                || (!close
                    && !force
                    && run
                        .generated_at
                        .is_some_and(|at| now >= at && now - at < 30_000))
            {
                return;
            }
            run.closed = close;
            run.generated_at = Some(now);
            run.sequence = run.sequence.saturating_add(1).min(9_007_199_254_740_991);
            Heartbeat {
                run_id: self.run_id.clone(),
                sequence: run.sequence,
                sent_at: utc(now),
                status: if close {
                    Status::Closed
                } else if run.instances.values().any(|i| i.busy) {
                    Status::Busy
                } else {
                    Status::Idle
                },
                identity: self.identity.clone(),
                last_activity_at: run.last_activity.map(utc),
                snapshots: run
                    .instances
                    .iter()
                    .filter_map(|(id, instance)| {
                        instance.revision.map(|revision| Snapshot {
                            record_id: id.clone(),
                            revision,
                        })
                    })
                    .take(512)
                    .collect(),
            }
        };
        join_all(endpoints.iter().filter(|e| !e.unavailable).map(|endpoint| {
            self.enqueue(
                Pending {
                    heartbeat: heartbeat.clone(),
                    at: now,
                    auth: auth.clone(),
                    endpoint: endpoint.clone(),
                },
                cancel.clone(),
            )
        }))
        .await;
    }
    async fn enqueue(&self, pending: Pending, cancel: CancellationToken) {
        let state = self
            .flights
            .lock()
            .expect("heartbeat flights")
            .entry(pending.endpoint.url.clone())
            .or_default()
            .clone();
        {
            let mut state = state.lock().expect("heartbeat flight");
            if pending.heartbeat.sequence <= state.latest_sequence {
                return;
            }
            state.latest_sequence = pending.heartbeat.sequence;
            state.pending = Some(pending);
            if state.running {
                return;
            }
            state.running = true;
        }
        let mut lease = Lease {
            state: state.clone(),
            active: true,
        };
        loop {
            // Empty detection and the running flag change share one lock, preventing lost wakeups.
            let pending = {
                let mut state = state.lock().expect("heartbeat flight");
                match state.pending.take() {
                    Some(p) => p,
                    None => {
                        state.running = false;
                        lease.active = false;
                        break;
                    }
                }
            };
            if cancel.is_cancelled() {
                break;
            }
            if self.sender.now().saturating_sub(pending.at) > 90_000 {
                continue;
            }
            let outcome = self
                .sender
                .send_with_fallback(
                    &pending.endpoint,
                    &pending.auth,
                    &[],
                    Some(&pending.heartbeat),
                    cancel.clone(),
                )
                .await;
            let event = if matches!(
                outcome,
                SendOutcome::Accepted {
                    heartbeat_acknowledged: true,
                    ..
                }
            ) {
                Event::Heartbeat {
                    endpoint: pending.endpoint.url,
                    run_id: pending.heartbeat.run_id,
                    sequence: pending.heartbeat.sequence,
                }
            } else {
                Event::Failure {
                    endpoint: Some(pending.endpoint.url),
                    kind: outcome.failure().unwrap_or(FailureKind::Protocol),
                }
            };
            self.diagnostics
                .record_async(event, self.sender.now(), cancel.clone())
                .await;
        }
        drop(lease);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::{
        identity::Installation,
        owner::Owner,
        transport::{Method, Request, Response, Transport},
    };
    use pi_ai::BoxFuture;
    use serde_json::{Value, json};

    #[derive(Default)]
    struct Echo(Mutex<Vec<u64>>);
    impl Transport for Echo {
        fn request(
            &self,
            request: Request,
            _: CancellationToken,
        ) -> BoxFuture<'_, Result<Response, FailureKind>> {
            Box::pin(async move {
                let (status, value) = if request.method == Method::Get {
                    (
                        200,
                        json!({"protocols":["0.2"],"capabilities":["session-snapshots","revision-acks","heartbeat"]}),
                    )
                } else {
                    let body: Value =
                        serde_json::from_slice(request.body.as_ref().unwrap()).unwrap();
                    self.0
                        .lock()
                        .unwrap()
                        .push(body["heartbeat"]["sequence"].as_u64().unwrap());
                    (
                        202,
                        json!({"v":"0.2","acks":[],"heartbeatAck":{"runId":body["heartbeat"]["runId"],"sequence":body["heartbeat"]["sequence"],"status":"stored"}}),
                    )
                };
                Ok(Response {
                    status,
                    body: serde_json::to_vec(&value).unwrap(),
                })
            })
        }
    }
    #[tokio::test]
    async fn late_older_pulses_do_not_replace_a_newer_endpoint_sequence() {
        let root =
            std::env::temp_dir().join(format!("cst-heartbeat-order-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let echo = Arc::new(Echo::default());
        let sender = Sender::new(echo.clone(), Arc::new(crate::auth::oa::SystemClock));
        let identity = Identity {
            installation_id: None,
            environment_id: None,
            environment_source: "unavailable".into(),
        };
        let run = HeartbeatRun::new(
            identity.clone(),
            sender,
            Diagnostics::new(&root, Owner::global().unwrap()).unwrap(),
        );
        let auth = UploadAuth::Installation(Installation::load(&root).unwrap());
        for sequence in [2, 1, 2, 3] {
            let now = pi_ai::now_ms();
            run.enqueue(
                Pending {
                    heartbeat: Heartbeat {
                        run_id: run.run_id.clone(),
                        sequence,
                        sent_at: utc(now),
                        status: Status::Idle,
                        identity: identity.clone(),
                        last_activity_at: None,
                        snapshots: vec![],
                    },
                    at: now,
                    auth: auth.clone(),
                    endpoint: Endpoint {
                        url: "https://receiver.test/api".into(),
                        ca: None,
                        unavailable: false,
                    },
                },
                CancellationToken::new(),
            )
            .await;
        }
        assert_eq!(*echo.0.lock().unwrap(), [2, 3]);
        std::fs::remove_dir_all(root).unwrap();
    }
}

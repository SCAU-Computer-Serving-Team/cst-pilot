use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use super::{
    identity::hex,
    outbox::QueuedRecord,
    owner::Owner,
    storage::LocalFiles,
    transport::{FailureKind, utc},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRef {
    pub record_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    #[serde(rename = "final", skip_serializing_if = "Option::is_none")]
    pub final_state: Option<bool>,
}
impl From<&QueuedRecord> for SnapshotRef {
    fn from(record: &QueuedRecord) -> Self {
        Self {
            record_id: record.id().into(),
            revision: record.value()["revision"].as_u64(),
            final_state: record.value()["final"].as_bool(),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct TimedRef {
    #[serde(flatten)]
    reference: SnapshotRef,
    at: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct Ack {
    #[serde(flatten)]
    reference: SnapshotRef,
    status: AckStatus,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum AckStatus {
    Confirmed,
    Rejected,
}
#[derive(Clone, Serialize, Deserialize)]
struct HeartbeatAck {
    #[serde(rename = "runId")]
    run_id: String,
    sequence: u64,
    #[serde(rename = "acknowledgedAt")]
    acknowledged_at: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct Failure {
    kind: FailureKind,
    at: String,
}
#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EndpointStatus {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    queued: Vec<TimedRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    acks: Vec<Ack>,
    #[serde(skip_serializing_if = "Option::is_none")]
    acknowledged_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    heartbeat: Option<HeartbeatAck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<Failure>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct State {
    v: u8,
    updated_at: String,
    saved: Vec<TimedRef>,
    endpoints: BTreeMap<String, EndpointStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<Failure>,
}
pub enum Event {
    Saved(SnapshotRef),
    Queued {
        endpoint: String,
        record: SnapshotRef,
    },
    Acks {
        endpoint: String,
        acknowledged: Vec<QueuedRecord>,
        rejected: Vec<QueuedRecord>,
    },
    Heartbeat {
        endpoint: String,
        run_id: String,
        sequence: u64,
    },
    Failure {
        endpoint: Option<String>,
        kind: FailureKind,
    },
}
#[derive(Clone)]
pub struct Diagnostics {
    files: LocalFiles,
}
impl Diagnostics {
    pub fn new(root: &Path, owner: Arc<Owner>) -> Result<Self, String> {
        Ok(Self {
            files: LocalFiles::new(root, owner)?,
        })
    }
    /// Best-effort bounded local status. Failure never changes delivery or queue deletion.
    pub fn record(&self, event: Event, now: u64, cancel: &CancellationToken) {
        let path = self.files.path("status.json");
        let _ = self.files.transaction(&path, cancel, || {
            let now = utc(now);
            let mut state = self
                .files
                .read(&path, 64 * 1024)?
                .and_then(|b| serde_json::from_slice::<State>(&b).ok())
                .filter(|s| s.v == 1)
                .unwrap_or(State {
                    v: 1,
                    updated_at: now.clone(),
                    saved: vec![],
                    endpoints: BTreeMap::new(),
                    failure: None,
                });
            state.updated_at = now.clone();
            match event {
                Event::Saved(reference) => {
                    state
                        .saved
                        .retain(|r| r.reference.record_id != reference.record_id);
                    state.saved.push(TimedRef {
                        reference,
                        at: now.clone(),
                    });
                }
                event => {
                    let url = match &event {
                        Event::Queued { endpoint, .. }
                        | Event::Acks { endpoint, .. }
                        | Event::Heartbeat { endpoint, .. } => Some(endpoint),
                        Event::Failure { endpoint, .. } => endpoint.as_ref(),
                        _ => None,
                    };
                    if let Some(url) = url {
                        let key = hex(&Sha256::digest(url.as_bytes()))[..16].to_owned();
                        let endpoint = state.endpoints.entry(key).or_default();
                        match event {
                            Event::Queued { record, .. } => {
                                endpoint
                                    .queued
                                    .retain(|r| r.reference.record_id != record.record_id);
                                endpoint.queued.push(TimedRef {
                                    reference: record,
                                    at: now.clone(),
                                });
                            }
                            Event::Acks {
                                acknowledged,
                                rejected,
                                ..
                            } if !acknowledged.is_empty() => {
                                endpoint.acks.retain(|r| {
                                    !acknowledged.iter().any(|a| a.id() == r.reference.record_id)
                                });
                                endpoint.acks.extend(acknowledged.iter().map(|r| Ack {
                                    reference: r.into(),
                                    status: if rejected.contains(r) {
                                        AckStatus::Rejected
                                    } else {
                                        AckStatus::Confirmed
                                    },
                                }));
                                endpoint.acknowledged_at = Some(now.clone());
                            }
                            Event::Heartbeat {
                                run_id, sequence, ..
                            } => {
                                endpoint.heartbeat = Some(HeartbeatAck {
                                    run_id,
                                    sequence,
                                    acknowledged_at: now.clone(),
                                })
                            }
                            Event::Failure { kind, .. } => {
                                endpoint.failure = Some(Failure {
                                    kind,
                                    at: now.clone(),
                                })
                            }
                            _ => (),
                        }
                    } else if let Event::Failure { kind, .. } = event {
                        state.failure = Some(Failure {
                            kind,
                            at: now.clone(),
                        });
                    }
                }
            }
            trim(&mut state.saved, 50);
            // Deserialization projects only the allowed fields; no raw URL or arbitrary prior JSON survives.
            state
                .endpoints
                .retain(|key, _| key.len() == 16 && key.bytes().all(|b| b.is_ascii_hexdigit()));
            while state.endpoints.len() > 8 {
                let key = state.endpoints.keys().next().cloned().expect("nonempty");
                state.endpoints.remove(&key);
            }
            for endpoint in state.endpoints.values_mut() {
                trim(&mut endpoint.queued, 20);
                trim(&mut endpoint.acks, 20);
            }
            let bytes = serde_json::to_vec(&state).map_err(|_| "遥测诊断编码失败。")?;
            if bytes.len() > 64 * 1024 {
                return Ok(());
            }
            self.files.write(&path, &bytes, cancel)
        });
    }
    pub async fn record_async(&self, event: Event, now: u64, cancel: CancellationToken) {
        let this = self.clone();
        let _ = tokio::task::spawn_blocking(move || this.record(event, now, &cancel)).await;
    }
}
fn trim<T>(items: &mut Vec<T>, limit: usize) {
    if items.len() > limit {
        items.drain(..items.len() - limit);
    }
}

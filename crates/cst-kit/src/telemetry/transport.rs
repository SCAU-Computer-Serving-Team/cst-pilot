use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_ai::{BoxFuture, Credential, CredentialStore};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use super::{
    config::{Endpoint, endpoint_url},
    identity::Installation,
    outbox::QueuedRecord,
};
use crate::auth::oa::{Clock, SystemClock};

pub const RESPONSE_MAX_BYTES: usize = 64 * 1024;
pub const POST_TIMEOUT: Duration = Duration::from_secs(5);
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FailureKind {
    Storage,
    Auth,
    Capability,
    Protocol,
    Network,
    Timeout,
    Tls,
    Http,
}

/// Secret-bearing authentication is never formatted or serialized.
#[derive(Clone)]
pub enum UploadAuth {
    Installation(Installation),
    Bearer {
        installation: Installation,
        access: Zeroizing<String>,
    },
    LegacyBearer(Zeroizing<String>),
}
impl UploadAuth {
    /// Passive OAuth inspection only. API keys and expired credentials are not upload tokens.
    pub fn from_store(
        installation: Option<Installation>,
        credentials: &dyn CredentialStore,
        provider: &str,
        now: u64,
    ) -> Option<Self> {
        let installation = installation?;
        let access = credentials
            .inspect(provider)
            .and_then(|item| match item.credential {
                Credential::OAuth(c) if c.expires > now && valid_access(&c.access) => {
                    Some(c.access)
                }
                _ => None,
            });
        Some(match access {
            Some(access) => Self::Bearer {
                installation,
                access: Zeroizing::new(access),
            },
            None => Self::Installation(installation),
        })
    }
    fn fallback(&self) -> Option<Self> {
        match self {
            Self::Bearer { installation, .. } => Some(Self::Installation(installation.clone())),
            _ => None,
        }
    }
}
fn valid_access(access: &str) -> bool {
    !access.is_empty() && access.len() <= 8192 && access.bytes().all(|c| c.is_ascii_graphic())
}
#[derive(Clone, Debug, PartialEq)]
pub enum SendOutcome {
    Accepted {
        acknowledged: Vec<QueuedRecord>,
        rejected: Vec<QueuedRecord>,
        heartbeat_acknowledged: bool,
    },
    Auth,
    Bad,
    Stop,
    Retry(FailureKind),
}
impl SendOutcome {
    pub fn failure(&self) -> Option<FailureKind> {
        match self {
            Self::Accepted { .. } => None,
            Self::Auth => Some(FailureKind::Auth),
            Self::Bad | Self::Stop => Some(FailureKind::Http),
            Self::Retry(kind) => Some(*kind),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}
pub struct Request {
    pub endpoint: Endpoint,
    pub method: Method,
    pub headers: BTreeMap<String, String>,
    pub body: Option<Vec<u8>>,
}
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}
pub trait Transport: Send + Sync {
    fn request(
        &self,
        request: Request,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<Response, FailureKind>>;
}
pub struct NativeTransport;
impl Transport for NativeTransport {
    fn request(
        &self,
        request: Request,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<Response, FailureKind>> {
        Box::pin(async move {
            if request.endpoint.unavailable || endpoint_url(&request.endpoint.url).is_none() {
                return Err(FailureKind::Tls);
            }
            let mut builder = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(POST_TIMEOUT);
            if let Some(ca) = &request.endpoint.ca {
                if !request.endpoint.url.starts_with("https:") {
                    return Err(FailureKind::Tls);
                }
                let cert =
                    reqwest::Certificate::from_pem(ca.as_bytes()).map_err(|_| FailureKind::Tls)?;
                builder = builder.tls_certs_only([cert]);
            }
            let client = builder.build().map_err(|_| FailureKind::Tls)?;
            let work = async {
                let mut req = client.request(
                    match request.method {
                        Method::Get => reqwest::Method::GET,
                        Method::Post => reqwest::Method::POST,
                    },
                    &request.endpoint.url,
                );
                for (name, value) in request.headers {
                    req = req.header(name, value);
                }
                if let Some(body) = request.body {
                    req = req.body(body);
                }
                let mut response = req.send().await.map_err(classify)?;
                if response
                    .content_length()
                    .is_some_and(|n| n > RESPONSE_MAX_BYTES as u64)
                {
                    return Err(FailureKind::Protocol);
                }
                let status = response.status().as_u16();
                let mut body = Vec::new();
                while let Some(chunk) = response.chunk().await.map_err(classify)? {
                    if body.len() + chunk.len() > RESPONSE_MAX_BYTES {
                        return Err(FailureKind::Protocol);
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(Response { status, body })
            };
            tokio::select! { biased; _ = cancel.cancelled() => Err(FailureKind::Timeout), result = work => result }
        })
    }
}
fn classify(error: reqwest::Error) -> FailureKind {
    if error.is_timeout() {
        return FailureKind::Timeout;
    }
    // Inspect the source locally, retaining only a fixed category, never the URL or token.
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&error);
    while let Some(error) = source {
        let text = error.to_string().to_ascii_lowercase();
        if ["certificate", "tls", "ssl"]
            .iter()
            .any(|needle| text.contains(needle))
        {
            return FailureKind::Tls;
        }
        source = error.source();
    }
    FailureKind::Network
}

#[derive(Clone)]
pub struct Sender {
    transport: Arc<dyn Transport>,
    clock: Arc<dyn Clock>,
    stopped: Arc<Mutex<BTreeSet<String>>>,
}
impl Default for Sender {
    fn default() -> Self {
        Self::new(Arc::new(NativeTransport), Arc::new(SystemClock))
    }
}
impl Sender {
    pub fn new(transport: Arc<dyn Transport>, clock: Arc<dyn Clock>) -> Self {
        Self {
            transport,
            clock,
            stopped: Arc::default(),
        }
    }
    pub fn now(&self) -> u64 {
        self.clock.now()
    }
    pub async fn send(
        &self,
        endpoint: &Endpoint,
        auth: &UploadAuth,
        records: &[QueuedRecord],
        heartbeat: Option<&super::heartbeat::Heartbeat>,
        cancel: CancellationToken,
    ) -> SendOutcome {
        if endpoint.unavailable {
            return SendOutcome::Retry(FailureKind::Tls);
        }
        if self
            .stopped
            .lock()
            .expect("stopped endpoints")
            .contains(&endpoint.url)
        {
            return SendOutcome::Stop;
        }
        let child = cancel.child_token();
        let _guard = child.clone().drop_guard();
        let work = self.attempt(endpoint, auth, records, heartbeat, child.clone());
        let outcome = tokio::select! { biased; _ = cancel.cancelled() => SendOutcome::Retry(FailureKind::Timeout), result = tokio::time::timeout(POST_TIMEOUT, work) => result.unwrap_or(SendOutcome::Retry(FailureKind::Timeout)) };
        if outcome == SendOutcome::Stop {
            self.stopped
                .lock()
                .expect("stopped endpoints")
                .insert(endpoint.url.clone());
        }
        outcome
    }
    /// A rejected Bearer is retried using the same installation identity and record versions.
    pub async fn send_with_fallback(
        &self,
        endpoint: &Endpoint,
        auth: &UploadAuth,
        records: &[QueuedRecord],
        heartbeat: Option<&super::heartbeat::Heartbeat>,
        cancel: CancellationToken,
    ) -> SendOutcome {
        let outcome = self
            .send(endpoint, auth, records, heartbeat, cancel.clone())
            .await;
        if outcome == SendOutcome::Auth
            && let Some(fallback) = auth.fallback()
        {
            return self
                .send(endpoint, &fallback, records, heartbeat, cancel)
                .await;
        }
        outcome
    }
    async fn attempt(
        &self,
        endpoint: &Endpoint,
        auth: &UploadAuth,
        records: &[QueuedRecord],
        heartbeat: Option<&super::heartbeat::Heartbeat>,
        cancel: CancellationToken,
    ) -> SendOutcome {
        if endpoint_url(&endpoint.url).is_none() {
            return SendOutcome::Retry(FailureKind::Http);
        }
        if records.len() > 50
            || records.iter().any(|record| {
                records
                    .first()
                    .is_some_and(|first| first.version() != record.version())
            })
        {
            return SendOutcome::Retry(FailureKind::Protocol);
        }
        let snapshots =
            heartbeat.is_some() || records.first().is_some_and(|r| r.version() == "0.2");
        if snapshots {
            let response = match self
                .transport
                .request(
                    Request {
                        endpoint: endpoint.clone(),
                        method: Method::Get,
                        headers: BTreeMap::new(),
                        body: None,
                    },
                    cancel.clone(),
                )
                .await
            {
                Ok(r) => r,
                Err(e) => return SendOutcome::Retry(e),
            };
            if matches!(response.status, 404 | 410) {
                return SendOutcome::Stop;
            }
            if response.status != 200 {
                return SendOutcome::Retry(FailureKind::Capability);
            }
            let value = match parse(&response.body) {
                Ok(v) => v,
                Err(e) => return SendOutcome::Retry(e),
            };
            let contains = |key: &str, item: &str| {
                value
                    .get(key)
                    .and_then(Value::as_array)
                    .is_some_and(|items| items.iter().any(|v| v.as_str() == Some(item)))
            };
            if !contains("protocols", "0.2")
                || !contains("capabilities", "session-snapshots")
                || !contains("capabilities", "revision-acks")
                || (heartbeat.is_some() && !contains("capabilities", "heartbeat"))
            {
                return SendOutcome::Retry(FailureKind::Capability);
            }
            if matches!(auth, UploadAuth::LegacyBearer(_)) {
                return SendOutcome::Auth;
            }
        }
        let body = json!({"v": if snapshots { "0.2" } else { records.first().map_or("0.1", QueuedRecord::version) }, "batchId":uuid::Uuid::new_v4().to_string(),"sentAt":utc(self.now()), "records":records.iter().map(QueuedRecord::value).collect::<Vec<_>>()});
        let mut body = body;
        if let Some(heartbeat) = heartbeat {
            body["heartbeat"] = match serde_json::to_value(heartbeat) {
                Ok(v) => v,
                Err(_) => return SendOutcome::Retry(FailureKind::Protocol),
            };
        }
        let body = match serde_json::to_vec(&body) {
            Ok(b) if b.len() <= 1024 * 1024 => b,
            _ => return SendOutcome::Retry(FailureKind::Protocol),
        };
        let mut headers = match auth {
            UploadAuth::Installation(installation) => installation.headers(&body, self.now()),
            UploadAuth::Bearer {
                installation,
                access,
            } => {
                if !valid_access(access) {
                    return SendOutcome::Auth;
                }
                let mut h = installation.headers(&body, self.now());
                h.insert(
                    "Authorization".into(),
                    format!("Bearer {}", access.as_str()),
                );
                h
            }
            UploadAuth::LegacyBearer(access) => {
                if !valid_access(access) {
                    return SendOutcome::Auth;
                }
                BTreeMap::from([(
                    "Authorization".into(),
                    format!("Bearer {}", access.as_str()),
                )])
            }
        };
        headers.insert("Content-Type".into(), "application/json".into());
        let response = match self
            .transport
            .request(
                Request {
                    endpoint: endpoint.clone(),
                    method: Method::Post,
                    headers,
                    body: Some(body),
                },
                cancel,
            )
            .await
        {
            Ok(r) => r,
            Err(e) => return SendOutcome::Retry(e),
        };
        match response.status {
            202 if snapshots => match parse(&response.body) {
                Ok(value) => {
                    let (acknowledged, rejected) = acknowledgments(&value, records);
                    let heartbeat_acknowledged = heartbeat.is_some_and(|h| {
                        value["v"] == "0.2"
                            && value["heartbeatAck"]["runId"] == h.run_id
                            && value["heartbeatAck"]["sequence"].as_u64() == Some(h.sequence)
                            && confirmed(&value["heartbeatAck"]["status"])
                    });
                    SendOutcome::Accepted {
                        acknowledged,
                        rejected,
                        heartbeat_acknowledged,
                    }
                }
                Err(e) => SendOutcome::Retry(e),
            },
            202 => SendOutcome::Accepted {
                acknowledged: records.to_vec(),
                rejected: vec![],
                heartbeat_acknowledged: false,
            },
            401 | 403 => SendOutcome::Auth,
            400 | 413 if !snapshots => SendOutcome::Bad,
            404 | 410 => SendOutcome::Stop,
            _ => SendOutcome::Retry(FailureKind::Http),
        }
    }
}
fn parse(body: &[u8]) -> Result<Value, FailureKind> {
    if body.len() > RESPONSE_MAX_BYTES {
        return Err(FailureKind::Protocol);
    }
    serde_json::from_slice::<Value>(body)
        .ok()
        .filter(Value::is_object)
        .ok_or(FailureKind::Protocol)
}
fn confirmed(status: &Value) -> bool {
    matches!(status.as_str(), Some("stored" | "duplicate" | "stale"))
}
fn acknowledgments(
    value: &Value,
    records: &[QueuedRecord],
) -> (Vec<QueuedRecord>, Vec<QueuedRecord>) {
    if value["v"] != "0.2" {
        return (vec![], vec![]);
    }
    let Some(acks) = value["acks"].as_array() else {
        return (vec![], vec![]);
    };
    let mut acknowledged = Vec::new();
    let mut rejected = Vec::new();
    for record in records {
        let mut matches = acks.iter().filter(|ack| {
            ack.get("recordId").and_then(Value::as_str) == Some(record.id())
                && ack.get("revision").and_then(Value::as_u64) == Some(record.revision())
        });
        let Some(ack) = matches.next() else {
            continue;
        };
        if matches.next().is_some() {
            continue;
        }
        let reject = ack["status"] == "rejected"
            && matches!(
                ack["reason"].as_str(),
                Some(
                    "bad-revision"
                        | "bad-final-state"
                        | "bad-record-id"
                        | "unknown-type"
                        | "too-many-elements"
                        | "installation-mismatch"
                        | "immutable-legacy-record"
                        | "owner-mismatch"
                        | "instance-mismatch"
                        | "already-final"
                        | "envelope-version-mismatch"
                )
            );
        if confirmed(&ack["status"]) || reject {
            acknowledged.push(record.clone());
        }
        if reject {
            rejected.push(record.clone());
        }
    }
    (acknowledged, rejected)
}
pub(crate) fn utc(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms.min(i64::MAX as u64) as i64)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

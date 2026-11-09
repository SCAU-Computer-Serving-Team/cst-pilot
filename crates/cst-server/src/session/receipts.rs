use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

use crate::locks::{StorageError, atomic_write_json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReceiptStatus {
    Started,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptRecord<T = Value> {
    pub signature: String,
    pub status: ReceiptStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<T>,
}

#[derive(Debug, Clone)]
pub enum ReceiptError {
    InvalidKey(String),
    Conflict(String),
    Corrupted(String),
    Storage(StorageError),
    Action(String),
}

impl ReceiptError {
    pub fn is_conflict(&self) -> bool {
        matches!(self, Self::Conflict(_))
    }
}

impl std::fmt::Display for ReceiptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidKey(msg)
            | Self::Conflict(msg)
            | Self::Corrupted(msg)
            | Self::Action(msg) => f.write_str(msg),
            Self::Storage(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ReceiptError {}

pub fn is_valid_idempotency_key(key: &str) -> bool {
    (1..=128).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn compute_request_signature(method: &str, pathname: &str, body: &Value) -> String {
    let canonical = serde_json::to_string(&(method, pathname, body)).unwrap_or_default();
    format!("{:x}", Sha256::digest(canonical.as_bytes()))
}

struct InFlightReceipt {
    signature: String,
    notify: Arc<Notify>,
    outcome: Mutex<Option<Result<Value, ReceiptError>>>,
}

struct ReceiptCompletion {
    receipt: Arc<InFlightReceipt>,
    finished: bool,
}

impl ReceiptCompletion {
    fn finish(&mut self, result: Result<Value, ReceiptError>) {
        *self
            .receipt
            .outcome
            .lock()
            .unwrap_or_else(|err| err.into_inner()) = Some(result);
        self.finished = true;
        self.receipt.notify.notify_waiters();
    }
}

impl Drop for ReceiptCompletion {
    fn drop(&mut self) {
        if !self.finished {
            // Retain the uncertain key in memory: a cancelled spawn_blocking write
            // can still publish its started receipt after this future is dropped.
            self.finish(Err(ReceiptError::Conflict(
                "前次操作状态不确定，请核查后重试".to_owned(),
            )));
        }
    }
}

/// Durable idempotency receipt store (`agent/home/web-inbox/receipts/<sha256(key)>.json`).
/// Persists `status: "started"` before executing side effects and updates to `status: "done"` on completion.
/// Never replays a crashed `started` receipt.
#[derive(Clone)]
pub struct MutationReceipts {
    root: PathBuf,
    inflight: Arc<Mutex<HashMap<String, Arc<InFlightReceipt>>>>,
}

impl MutationReceipts {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            inflight: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn file_for_key(&self, key: &str) -> PathBuf {
        let hash = Sha256::digest(key.as_bytes());
        self.root.join(format!("{hash:x}.json"))
    }

    pub async fn lookup<T: DeserializeOwned>(
        &self,
        key: &str,
    ) -> Result<Option<ReceiptRecord<T>>, ReceiptError> {
        if !is_valid_idempotency_key(key) {
            return Err(ReceiptError::InvalidKey("去重键无效".to_owned()));
        }
        let path = self.file_for_key(key);
        let raw = match Self::read_raw(&path).await? {
            Some(r) => r,
            None => return Ok(None),
        };
        let typed_result = match raw.result {
            Some(val) => Some(
                serde_json::from_value(val)
                    .map_err(|_| ReceiptError::Corrupted("操作记录结果格式无效".to_owned()))?,
            ),
            None => None,
        };
        Ok(Some(ReceiptRecord {
            signature: raw.signature,
            status: raw.status,
            result: typed_result,
        }))
    }

    /// Executes `action` at most once per `key`. Cancellation wakes duplicate callers
    /// with a conflict and preserves the uncertain key without replaying side effects.
    pub async fn once_value<F, Fut>(
        &self,
        key: &str,
        signature: &str,
        action: F,
    ) -> Result<Value, ReceiptError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Value, ReceiptError>>,
    {
        self.once_value_with_acceptance(key, signature, (), action)
            .await
    }

    /// Releases channel admission after durable acceptance, or before waiting on an owner.
    pub(crate) async fn once_value_with_acceptance<G, F, Fut>(
        &self,
        key: &str,
        signature: &str,
        acceptance: G,
        action: F,
    ) -> Result<Value, ReceiptError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Value, ReceiptError>>,
    {
        if !is_valid_idempotency_key(key) {
            return Err(ReceiptError::InvalidKey("缺少有效的去重键".to_owned()));
        }

        let file = self.file_for_key(key);

        // Coordinate concurrent requests with the same key in memory
        let (inflight, is_owner) = {
            let mut map = self.inflight.lock().expect("inflight receipts");
            if let Some(inflight) = map.get(key).cloned() {
                (inflight, false)
            } else {
                let entry = Arc::new(InFlightReceipt {
                    signature: signature.to_owned(),
                    notify: Arc::new(Notify::new()),
                    outcome: Mutex::new(None),
                });
                map.insert(key.to_owned(), entry.clone());
                (entry, true)
            }
        };
        if inflight.signature != signature {
            return Err(ReceiptError::Conflict("去重键已用于其他操作".to_owned()));
        }

        if !is_owner {
            drop(acceptance);
            loop {
                let notified = inflight.notify.notified();
                let maybe_outcome = inflight.outcome.lock().expect("inflight outcome").clone();
                if let Some(outcome) = maybe_outcome {
                    let value = outcome?;
                    let saved = Self::read_raw(&file).await?;
                    if saved.as_ref().map(|s| s.signature.as_str()) != Some(signature) {
                        return Err(ReceiptError::Conflict("去重键已用于其他操作".to_owned()));
                    }
                    return Ok(value);
                }
                notified.await;
            }
        }

        let mut completion = ReceiptCompletion {
            receipt: inflight,
            finished: false,
        };
        let run_result = async {
            if let Some(saved) = Self::read_raw(&file).await? {
                if saved.signature != signature {
                    return Err(ReceiptError::Conflict("去重键已用于其他操作".to_owned()));
                }
                if saved.status != ReceiptStatus::Done {
                    return Err(ReceiptError::Conflict(
                        "前次操作状态不确定，请核查后重试".to_owned(),
                    ));
                }
                return Ok(saved.result.unwrap_or(Value::Null));
            }

            let started = ReceiptRecord::<Value> {
                signature: signature.to_owned(),
                status: ReceiptStatus::Started,
                result: None,
            };
            atomic_write_json(&file, &started, "写入幂等回执")
                .await
                .map_err(ReceiptError::Storage)?;

            drop(acceptance);
            let result = action().await?;
            let done = ReceiptRecord::<Value> {
                signature: signature.to_owned(),
                status: ReceiptStatus::Done,
                result: Some(result.clone()),
            };
            atomic_write_json(&file, &done, "完成幂等回执")
                .await
                .map_err(ReceiptError::Storage)?;
            Ok(result)
        }
        .await;

        completion.finish(run_result.clone());
        self.inflight.lock().expect("inflight receipts").remove(key);
        run_result
    }

    async fn read_raw(file: &Path) -> Result<Option<ReceiptRecord<Value>>, ReceiptError> {
        let path = file.to_path_buf();
        tokio::task::spawn_blocking(move || match fs::read(&path) {
            Ok(bytes) => {
                let parsed: Value = serde_json::from_slice(&bytes)
                    .map_err(|_| ReceiptError::Corrupted("操作记录损坏".to_owned()))?;
                let Some(obj) = parsed.as_object() else {
                    return Err(ReceiptError::Corrupted("操作记录损坏".to_owned()));
                };
                let Some(sig) = obj.get("signature").and_then(Value::as_str) else {
                    return Err(ReceiptError::Corrupted("操作记录损坏".to_owned()));
                };
                let status = match obj.get("status").and_then(Value::as_str) {
                    Some("started") => ReceiptStatus::Started,
                    Some("done") => ReceiptStatus::Done,
                    _ => return Err(ReceiptError::Corrupted("操作记录损坏".to_owned())),
                };
                Ok(Some(ReceiptRecord {
                    signature: sig.to_owned(),
                    status,
                    result: obj.get("result").cloned(),
                }))
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(ReceiptError::Storage(StorageError::from_io(
                &err,
                "读取幂等回执",
            ))),
        })
        .await
        .map_err(|_| ReceiptError::Action("读取幂等回执任务异常结束".to_owned()))?
    }
}

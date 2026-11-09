use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{
    billing::{Billing, BillingLookup, BillingRequest},
    outbox::{MAX_RECORD_BYTES, QueuedRecord},
    owner::{Owner, Proof},
    record::local_iso,
    storage::{LocalFiles, encode_name},
};

const DRAFT_BYTES: usize = 2 * MAX_RECORD_BYTES;
#[derive(Clone)]
pub struct Drafts {
    files: LocalFiles,
}
struct Candidate {
    raw: Vec<u8>,
    draft: Value,
}
impl Drafts {
    pub fn new(root: &Path, owner: Arc<Owner>) -> Result<Self, String> {
        Ok(Self {
            files: LocalFiles::new(root, owner)?,
        })
    }
    pub fn path(&self, id: &str) -> Result<PathBuf, String> {
        Ok(self
            .files
            .path(&format!("drafts/{}.json", encode_name(id)?)))
    }
    /// No await or external lookup while the short per-file transaction is held.
    pub fn save(
        &self,
        record: &QueuedRecord,
        last_activity: u64,
        requests: &[BillingRequest],
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        let sealed = record.value()["final"].as_bool().unwrap_or(false)
            || !record.value()["endReason"]
                .as_str()
                .unwrap_or("")
                .is_empty();
        self.save_snapshot(record, last_activity, requests, sealed, cancel)
    }
    pub(crate) fn save_snapshot(
        &self,
        record: &QueuedRecord,
        last_activity: u64,
        requests: &[BillingRequest],
        sealed: bool,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        let path = self.path(record.id())?;
        let value = json!({"record":record.value(),"lastTurnEndedAt":last_activity,"ownerPid":std::process::id(),"owner":self.files.owner.proof(),"cstoaRequests":requests,"sealed":sealed});
        let bytes = serde_json::to_vec(&value).map_err(|_| "遥测草稿无法编码。")?;
        if bytes.len() > DRAFT_BYTES {
            return Err("遥测草稿超过大小限制。".into());
        }
        self.files.transaction(&path, cancel, || {
            if let Some(old) = self.files.read(&path, DRAFT_BYTES)? {
                let previous: Value =
                    serde_json::from_slice(&old).map_err(|_| "已有遥测草稿损坏。")?;
                if previous.pointer("/record/recordId").and_then(Value::as_str) != Some(record.id())
                {
                    return Err("已有草稿记录标识不匹配。".into());
                }
                if previous
                    .get("owner")
                    .and_then(|value| serde_json::from_value::<Proof>(value.clone()).ok())
                    .is_some_and(|proof| {
                        proof.token != self.files.owner.proof().token && proof.alive()
                    })
                {
                    return Err("遥测草稿属于另一个活跃实例。".into());
                }
                if previous
                    .pointer("/record/revision")
                    .and_then(Value::as_u64)
                    .is_some_and(|v| v > record.revision())
                {
                    return Ok(());
                }
                if record.version() == "0.2"
                    && previous.pointer("/record/revision").and_then(Value::as_u64)
                        == Some(record.revision())
                    && previous.get("record") != Some(record.value())
                {
                    return Err("同一遥测版本的内容不能改写。".into());
                }
                if previous.pointer("/record/final").and_then(Value::as_bool) == Some(true)
                    && !record.value()["final"].as_bool().unwrap_or(false)
                {
                    return Err("已定稿草稿不能恢复为中间快照。".into());
                }
            }
            self.files.write(&path, &bytes, cancel)
        })
    }
    pub fn remove_if(
        &self,
        record: &QueuedRecord,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        let path = self.path(record.id())?;
        self.files.transaction(&path, cancel, || {
            if let Some(bytes) = self.files.read(&path, DRAFT_BYTES)? {
                let draft: Value = serde_json::from_slice(&bytes).map_err(|_| "遥测草稿损坏。")?;
                if draft.get("record") == Some(record.value()) {
                    self.files.remove_exact(&path, &bytes)?;
                }
            }
            Ok(())
        })
    }
    fn candidate(
        &self,
        path: &Path,
        cancel: &CancellationToken,
    ) -> Result<Option<Candidate>, String> {
        self.files.transaction(path, cancel, || {
            let Some(raw) = self.files.read(path, DRAFT_BYTES)? else {
                return Ok(None);
            };
            let draft: Value = serde_json::from_slice(&raw).map_err(|_| "遥测草稿损坏。")?;
            if draft
                .get("owner")
                .and_then(|value| serde_json::from_value::<Proof>(value.clone()).ok())
                .is_some_and(|proof| proof.alive())
            {
                return Ok(None);
            }
            let record = draft
                .get("record")
                .filter(|value| value.is_object())
                .ok_or("遥测草稿缺少记录。")?;
            // Unknown or damaged records are retained even when counters are absent.
            QueuedRecord::new(record.clone())?;
            if ![
                record.get("prompts"),
                record.get("turns"),
                record.pointer("/cstoaBilling/requests"),
            ]
            .iter()
            .any(|value| value.and_then(Value::as_u64).is_some_and(|v| v > 0))
            {
                self.files.remove_exact(path, &raw)?;
                return Ok(None);
            }
            Ok(Some(Candidate { raw, draft }))
        })
    }
    fn seal(
        &self,
        path: &Path,
        candidate: &Candidate,
        record: &QueuedRecord,
        cancel: &CancellationToken,
    ) -> Result<Option<Vec<u8>>, String> {
        self.files.transaction(path, cancel, || {
            if self.files.read(path, DRAFT_BYTES)?.as_deref() != Some(candidate.raw.as_slice()) {
                return Ok(None);
            }
            let mut value = candidate.draft.clone();
            let object = value.as_object_mut().ok_or("遥测草稿格式无效。")?;
            object.remove("owner");
            object.insert("sealed".into(), json!(true));
            object.insert("record".into(), record.value().clone());
            let bytes = serde_json::to_vec(&value).map_err(|_| "遥测草稿无法编码。")?;
            self.files.write(path, &bytes, cancel)?;
            Ok(Some(bytes))
        })
    }
    /// Freeze recovered content before enqueue. Retries reuse the sealed revision and old environment.
    pub async fn recover<F, Fut>(
        &self,
        persist: F,
        lookup: Option<&dyn BillingLookup>,
        cancel: CancellationToken,
    ) -> Result<Vec<QueuedRecord>, String>
    where
        F: Fn(QueuedRecord) -> Fut,
        Fut: Future<Output = Result<(), String>>,
    {
        let this = self.clone();
        let paths = tokio::task::spawn_blocking(move || this.list())
            .await
            .map_err(|_| "遥测草稿扫描中断。")??;
        let mut recovered = Vec::new();
        for path in paths {
            if cancel.is_cancelled() {
                break;
            }
            let this = self.clone();
            let target = path.clone();
            let token = cancel.clone();
            let candidate =
                tokio::task::spawn_blocking(move || this.candidate(&target, &token)).await;
            let Ok(Ok(Some(candidate))) = candidate else {
                continue;
            };
            let Ok(mut value) = crash_value(
                candidate.draft["record"].clone(),
                candidate.draft["lastTurnEndedAt"].as_u64().unwrap_or(0),
            ) else {
                continue;
            };
            // Unmarked final drafts are already sealed snapshots from compatible writers.
            let sealed = candidate.draft["sealed"].as_bool().unwrap_or_else(|| {
                !candidate.draft["record"]["endReason"]
                    .as_str()
                    .unwrap_or("")
                    .is_empty()
            });
            if !sealed && let Some(lookup) = lookup {
                let requests = candidate
                    .draft
                    .get("cstoaRequests")
                    .and_then(|raw| serde_json::from_value::<Vec<BillingRequest>>(raw.clone()).ok())
                    .unwrap_or_default();
                let requests = requests
                    .into_iter()
                    .filter(|item| {
                        item.request_id.as_ref().is_none_or(|id| {
                            !id.is_empty()
                                && id.len() <= 256
                                && id.bytes().all(|c| c.is_ascii_graphic())
                        }) && item.quota_points.is_none_or(|v| v <= 9_007_199_254_740_991)
                    })
                    .collect();
                let billing = Billing::restore(requests);
                billing
                    .settle(lookup, cancel.clone(), Duration::from_millis(4500))
                    .await;
                if let Some(summary) = billing.summary() {
                    let summary =
                        serde_json::to_value(summary).map_err(|_| "遥测扣点汇总无法编码。")?;
                    if value["cstoaBilling"] != summary
                        && value["v"] == "0.2"
                        && value["revision"] == candidate.draft["record"]["revision"]
                    {
                        let revision = value["revision"]
                            .as_u64()
                            .unwrap_or(0)
                            .checked_add(1)
                            .filter(|v| *v <= 9_007_199_254_740_991);
                        let Some(revision) = revision else {
                            continue;
                        };
                        value["revision"] = json!(revision);
                    }
                    value["cstoaBilling"] = summary;
                }
            }
            let Ok(record) = QueuedRecord::new(value) else {
                continue;
            };
            let this = self.clone();
            let target = path.clone();
            let token = cancel.clone();
            let output = record.clone();
            let sealed = tokio::task::spawn_blocking(move || {
                this.seal(&target, &candidate, &output, &token)
            })
            .await;
            let Ok(Ok(Some(bytes))) = sealed else {
                continue;
            };
            if persist(record.clone()).await.is_err() {
                continue;
            }
            recovered.push(record);
            let this = self.clone();
            let token = cancel.clone();
            let _ = tokio::task::spawn_blocking(move || {
                this.files
                    .transaction(&path, &token, || this.files.remove_exact(&path, &bytes))
            })
            .await;
        }
        Ok(recovered)
    }
    fn list(&self) -> Result<Vec<PathBuf>, String> {
        let path = self.files.path("drafts");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let _lease = self
            .files
            .base
            .pin_directory(Path::new("agent/home/telemetry/drafts"), false)?;
        let mut files = Vec::new();
        for entry in std::fs::read_dir(path).map_err(|_| "遥测草稿目录无法读取。")? {
            let entry = entry.map_err(|_| "遥测草稿条目无法读取。")?;
            if entry.path().extension().is_some_and(|ext| ext == "json") {
                files.push(entry.path());
            }
            if files.len() >= 10_000 {
                break;
            }
        }
        Ok(files)
    }
}
fn crash_value(mut value: Value, last_activity: u64) -> Result<Value, String> {
    if !value["endReason"].as_str().unwrap_or("").is_empty() {
        return Ok(value);
    }
    let started = value["startedAt"]
        .as_str()
        .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
        .map(|at| at.timestamp_millis())
        .filter(|at| *at >= 0)
        .ok_or("遥测草稿起始时间无效。")? as u64;
    let ended = last_activity.max(started);
    if value["recordId"].as_str().is_none_or(str::is_empty) {
        value["recordId"] = json!(uuid::Uuid::new_v4().to_string());
    }
    value["endReason"] = json!("crash");
    value["endedAt"] = json!(local_iso(ended));
    value["durationMs"] = json!(ended - started);
    if value["v"] == "0.2" {
        let revision = value["revision"]
            .as_u64()
            .unwrap_or(0)
            .checked_add(1)
            .filter(|v| *v <= 9_007_199_254_740_991)
            .ok_or("遥测草稿版本超过上限。")?;
        value["revision"] = json!(revision);
        value["final"] = json!(true);
    }
    Ok(value)
}

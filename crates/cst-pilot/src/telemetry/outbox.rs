use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use super::{identity::hex, owner::Owner, storage::LocalFiles};

pub const MAX_RECORDS: usize = 200;
pub const MAX_RECORD_BYTES: usize = 1024 * 1024;
pub const BATCH_RECORDS: usize = 50;
pub const BATCH_BYTES: usize = 900 * 1024;
const MAX_QUEUE_BYTES: usize = (MAX_RECORDS + 1) * (MAX_RECORD_BYTES + 1);
const MAX_SAFE: u64 = 9_007_199_254_740_991;

/// Value retains legacy fields and unknown forward-compatible record fields.
#[derive(Clone, Debug, PartialEq)]
pub struct QueuedRecord(Value);
impl QueuedRecord {
    pub fn new(value: Value) -> Result<Self, String> {
        let id = value
            .get("recordId")
            .and_then(Value::as_str)
            .ok_or("遥测记录缺少标识。")?;
        if id.is_empty()
            || id.len() > 256
            || id.chars().any(char::is_control)
            || value.get("type").and_then(Value::as_str) != Some("session")
            || !matches!(value.get("v").and_then(Value::as_str), Some("0.1" | "0.2"))
            || (value["v"] == "0.2"
                && (value
                    .get("revision")
                    .and_then(Value::as_u64)
                    .is_none_or(|v| v == 0 || v > MAX_SAFE)
                    || !value.get("final").is_some_and(Value::is_boolean)))
        {
            return Err("遥测记录格式无效。".into());
        }
        if serde_json::to_vec(&value)
            .map_err(|_| "遥测记录无法编码。")?
            .len()
            > MAX_RECORD_BYTES
        {
            return Err("遥测记录超过 1 MiB，草稿将保留。".into());
        }
        Ok(Self(value))
    }
    pub fn value(&self) -> &Value {
        &self.0
    }
    pub fn into_value(self) -> Value {
        self.0
    }
    pub fn id(&self) -> &str {
        self.0["recordId"].as_str().expect("validated id")
    }
    pub fn revision(&self) -> u64 {
        self.0["revision"].as_u64().unwrap_or(0)
    }
    pub fn version(&self) -> &str {
        self.0["v"].as_str().expect("validated version")
    }
}
#[derive(Clone)]
pub struct Outbox {
    files: LocalFiles,
}
struct Contents {
    records: Vec<QueuedRecord>,
    corrupt: Vec<Vec<u8>>,
}
impl Outbox {
    pub fn new(root: &Path, owner: Arc<Owner>) -> Result<Self, String> {
        Ok(Self {
            files: LocalFiles::new(root, owner)?,
        })
    }
    pub fn path(&self, endpoint: Option<&str>) -> PathBuf {
        self.files.path(&endpoint.map_or_else(
            || "pending.jsonl".into(),
            |url| format!("pending-{}.jsonl", hex(&Sha256::digest(url.as_bytes()))),
        ))
    }
    fn contents(&self, path: &Path) -> Result<Contents, String> {
        let bytes = self.files.read(path, MAX_QUEUE_BYTES)?.unwrap_or_default();
        let mut contents = Contents {
            records: Vec::new(),
            corrupt: Vec::new(),
        };
        for line in bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
        {
            let parsed = if line.len() <= MAX_RECORD_BYTES {
                serde_json::from_slice(line)
                    .ok()
                    .and_then(|value| QueuedRecord::new(value).ok())
            } else {
                None
            };
            if let Some(record) = parsed {
                contents.records.push(record);
            } else {
                contents.corrupt.push(line.to_vec());
            }
        }
        Ok(contents)
    }
    fn rewrite(
        &self,
        path: &Path,
        mut contents: Contents,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        // Preserve malformed original bytes locally before removing them from the upload queue.
        if !contents.corrupt.is_empty() {
            let mut bytes = Vec::new();
            for line in contents.corrupt {
                bytes.extend(line);
                bytes.push(b'\n');
            }
            let quarantine = path.with_file_name(format!(
                "{}.corrupt-{}",
                path.file_name().unwrap_or_default().to_string_lossy(),
                uuid::Uuid::new_v4()
            ));
            self.files.base.atomic(&quarantine, &bytes, false, cancel)?;
        }
        if contents.records.len() > MAX_RECORDS {
            contents
                .records
                .drain(..contents.records.len() - MAX_RECORDS);
        }
        let mut bytes = Vec::new();
        for record in contents.records {
            serde_json::to_writer(&mut bytes, record.value()).map_err(|_| "遥测队列无法编码。")?;
            bytes.push(b'\n');
        }
        self.files.write(path, &bytes, cancel)
    }
    pub fn append(
        &self,
        record: &QueuedRecord,
        endpoint: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<QueuedRecord, String> {
        let path = self.path(endpoint);
        self.files.transaction(&path, cancel, || {
            let mut contents = self.contents(&path)?;
            if let Some(current) = contents
                .records
                .iter()
                .filter(|item| {
                    item.id() == record.id() && item.version() == "0.2" && record.version() == "0.2"
                })
                .max_by_key(|item| item.revision())
                && current.revision() >= record.revision()
            {
                return Ok(current.clone());
            }
            contents.records.retain(|item| item.id() != record.id());
            contents.records.push(record.clone());
            self.rewrite(&path, contents, cancel)?;
            Ok(record.clone())
        })
    }
    pub fn read_all(&self, endpoint: Option<&str>) -> Result<Vec<QueuedRecord>, String> {
        Ok(self.contents(&self.path(endpoint))?.records)
    }
    /// Exact sent content is the deletion key; an in-flight older ack cannot remove an update.
    pub fn acknowledge(
        &self,
        sent: &[QueuedRecord],
        endpoint: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        let path = self.path(endpoint);
        self.files.transaction(&path, cancel, || {
            let mut contents = self.contents(&path)?;
            contents.records.retain(|item| !sent.contains(item));
            self.rewrite(&path, contents, cancel)
        })
    }
    /// Partial failure leaves successful endpoints intact and the caller's draft recoverable.
    pub fn queue_all(
        &self,
        record: &QueuedRecord,
        endpoints: &[String],
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        let mut failed = false;
        for endpoint in endpoints {
            failed |= self.append(record, Some(endpoint), cancel).is_err();
        }
        if failed || endpoints.is_empty() {
            Err("遥测队列未全部保存，恢复草稿将保留。".into())
        } else {
            Ok(())
        }
    }
    pub fn migrate_legacy(
        &self,
        endpoints: &[String],
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        if endpoints.is_empty() {
            return Ok(());
        }
        let records = self.read_all(None)?;
        for record in &records {
            self.queue_all(record, endpoints, cancel)?;
        }
        self.acknowledge(&records, None, cancel)
    }
    /// Preserve an unsendable head locally before removing it within the queue transaction.
    pub fn quarantine_head(
        &self,
        endpoint: &str,
        cancel: &CancellationToken,
    ) -> Result<bool, String> {
        let path = self.path(Some(endpoint));
        self.files.transaction(&path, cancel, || {
            let mut contents = self.contents(&path)?;
            let Some(first) = contents.records.first() else {
                return Ok(false);
            };
            let bytes = serde_json::to_vec(first.value()).map_err(|_| "遥测记录无法编码。")?;
            if bytes.len() <= BATCH_BYTES {
                return Ok(false);
            }
            let quarantine = path.with_file_name(format!(
                "{}.oversize-{}",
                path.file_name().unwrap_or_default().to_string_lossy(),
                uuid::Uuid::new_v4()
            ));
            self.files.base.atomic(&quarantine, &bytes, false, cancel)?;
            contents.records.remove(0);
            self.rewrite(&path, contents, cancel)?;
            Ok(true)
        })
    }
    pub fn batch(&self, endpoint: &str) -> Result<Vec<QueuedRecord>, String> {
        let records = self.read_all(Some(endpoint))?;
        let mut batch = Vec::new();
        let mut bytes = 0;
        for record in records {
            if batch
                .first()
                .is_some_and(|first: &QueuedRecord| first.version() != record.version())
            {
                break;
            }
            let size = serde_json::to_vec(record.value())
                .map_err(|_| "遥测记录无法编码。")?
                .len();
            if batch.len() >= BATCH_RECORDS || bytes + size > BATCH_BYTES {
                break;
            }
            bytes += size;
            batch.push(record);
        }
        Ok(batch)
    }
}

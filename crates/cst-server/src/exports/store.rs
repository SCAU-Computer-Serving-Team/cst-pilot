use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use chrono::{DateTime, Local, Timelike};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::discovery::strip_windows_verbatim_prefix;
use crate::error::{ServiceError, ServiceErrorKind};
use crate::exports::open::{
    artifact_sessions_display_path, ensure_artifact_sessions_dir, existing_artifact_sessions_dir,
};
use crate::exports::render::ExportRenderer;
use crate::exports::snapshot::{
    ExportFileEntry, ExportJobRecord, artifact_name, parse_export_options, validate_artifact_date,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredFileRecord {
    pub name: String,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRecord {
    pub result: ExportJobRecord,
    pub stored: Vec<StoredFileRecord>,
}

#[derive(Debug, Clone)]
pub struct ExportDownloadedFile {
    pub name: String,
    pub data: Vec<u8>,
    pub content_type: &'static str,
}

struct WorkItem {
    id: String,
    snapshot: Value,
    state: Mutex<(ExportRecord, ExportJobRecord)>,
}

impl WorkItem {
    fn visible_record(&self) -> ExportRecord {
        let guard = self.state.lock().expect("work item state lock");
        ExportRecord {
            result: guard.1.clone(),
            stored: guard.0.stored.clone(),
        }
    }
}

fn is_valid_export_id(id: &str) -> bool {
    static ID_RE: OnceLock<Regex> = OnceLock::new();
    let re =
        ID_RE.get_or_init(|| Regex::new(r"^[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}$").unwrap());
    re.is_match(id)
}

fn export_base_stem(snapshot: &Value) -> String {
    let title = snapshot
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("会话");
    let date_compact = snapshot
        .get("options")
        .and_then(|o| o.get("date"))
        .and_then(Value::as_str)
        .unwrap_or("1970-01-01")
        .replace('-', "");
    let captured_at = snapshot
        .get("capturedAt")
        .and_then(Value::as_str)
        .unwrap_or("");
    let clock = DateTime::parse_from_rfc3339(captured_at)
        .map(|dt| {
            let local = dt.with_timezone(&Local);
            format!(
                "{:02}{:02}{:02}",
                local.hour(),
                local.minute(),
                local.second()
            )
        })
        .unwrap_or_else(|_| "000000".to_owned());
    format!("{}-{date_compact}-{clock}", artifact_name(title))
}

fn is_enospc(err: &io::Error) -> bool {
    err.raw_os_error() == Some(112) || err.raw_os_error() == Some(28)
}

fn copy_file_exclusive(src: &Path, dst: &Path) -> io::Result<()> {
    let mut reader = fs::File::open(src)?;
    let mut writer = OpenOptions::new().write(true).create_new(true).open(dst)?;
    if let Err(err) = io::copy(&mut reader, &mut writer).and_then(|_| writer.flush()) {
        drop(writer);
        let _ = fs::remove_file(dst);
        return Err(err);
    }
    Ok(())
}

struct QueueState {
    queue: VecDeque<Arc<WorkItem>>,
    active: Option<Arc<WorkItem>>,
    active_cancel: Option<CancellationToken>,
    active_cancel_error: Option<ServiceError>,
    reservations: usize,
    pumping: Option<JoinHandle<()>>,
}

#[derive(Clone)]
pub struct SessionExportStore {
    root: PathBuf,
    metadata: PathBuf,
    work_initialized: Arc<Mutex<bool>>,
    renderer: Arc<dyn ExportRenderer>,
    inner: Arc<Mutex<QueueState>>,
    revision: Arc<AtomicU64>,
    closing: Arc<AtomicBool>,
    creating_count: Arc<AtomicUsize>,
    creating_done: Arc<Notify>,
    close_lock: Arc<AsyncMutex<bool>>,
}

impl SessionExportStore {
    pub fn new(
        root: impl Into<PathBuf>,
        metadata: impl Into<PathBuf>,
        renderer: Arc<dyn ExportRenderer>,
    ) -> Self {
        Self {
            root: root.into(),
            metadata: metadata.into(),
            work_initialized: Arc::new(Mutex::new(false)),
            renderer,
            inner: Arc::new(Mutex::new(QueueState {
                queue: VecDeque::new(),
                active: None,
                active_cancel: None,
                active_cancel_error: None,
                reservations: 0,
                pumping: None,
            })),
            revision: Arc::new(AtomicU64::new(0)),
            closing: Arc::new(AtomicBool::new(false)),
            creating_count: Arc::new(AtomicUsize::new(0)),
            creating_done: Arc::new(Notify::new()),
            close_lock: Arc::new(AsyncMutex::new(false)),
        }
    }

    pub fn set_origin(&self, origin: &str) {
        self.renderer.set_origin(origin);
    }

    fn clear_work_dir_sync(&self) -> Result<(), ServiceError> {
        match fs::remove_dir_all(self.metadata.join("work")) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(ServiceError::new(
                ServiceErrorKind::Unavailable,
                "export_failed",
                "导出临时文件无法清理，请关闭遗留导出浏览器并检查工具包写入权限后重试",
            )),
        }
    }

    fn initialize_work_dir_sync(&self) -> Result<(), ServiceError> {
        let mut initialized = self
            .work_initialized
            .lock()
            .expect("export work initialization lock");
        if !*initialized {
            self.clear_work_dir_sync()?;
            *initialized = true;
        }
        Ok(())
    }

    fn save_record_sync(&self, record: &ExportRecord) -> Result<(), ServiceError> {
        fs::create_dir_all(&self.metadata).map_err(|err| {
            let full = is_enospc(&err);
            ServiceError::new(
                if full {
                    ServiceErrorKind::StorageFull
                } else {
                    ServiceErrorKind::Internal
                },
                if full {
                    "storage_full"
                } else {
                    "export_record_unavailable"
                },
                if full {
                    "工具包存储空间不足，请清理后重试"
                } else {
                    "导出任务无法保存，请检查工具包写入权限"
                },
            )
        })?;
        let tmp = self
            .metadata
            .join(format!("{}.{}.tmp", record.result.id, Uuid::new_v4()));
        let target = self.metadata.join(format!("{}.json", record.result.id));
        let encoded = serde_json::to_vec(record).map_err(|_| {
            ServiceError::new(
                ServiceErrorKind::Internal,
                "export_record_unavailable",
                "导出任务无法保存，请检查工具包写入权限",
            )
        })?;
        let write_res = (|| -> io::Result<()> {
            let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            f.write_all(&encoded)?;
            f.sync_all()?;
            drop(f);
            fs::rename(&tmp, &target)
        })();
        let _ = fs::remove_file(&tmp);
        write_res.map_err(|err| {
            let full = is_enospc(&err);
            ServiceError::new(
                if full {
                    ServiceErrorKind::StorageFull
                } else {
                    ServiceErrorKind::Internal
                },
                if full {
                    "storage_full"
                } else {
                    "export_record_unavailable"
                },
                if full {
                    "工具包存储空间不足，请清理后重试"
                } else {
                    "导出任务无法保存，请检查工具包写入权限"
                },
            )
        })
    }

    fn commit_work_sync(&self, work: &WorkItem) -> Result<(), ServiceError> {
        let record_to_save = {
            let guard = work.state.lock().expect("work state lock");
            guard.0.clone()
        };
        self.save_record_sync(&record_to_save)?;
        let mut guard = work.state.lock().expect("work state lock");
        guard.1 = guard.0.result.clone();
        Ok(())
    }

    pub async fn create(&self, snapshot: &Value) -> Result<ExportJobRecord, ServiceError> {
        self.creating_count.fetch_add(1, Ordering::SeqCst);
        let res = self.enqueue(snapshot).await;
        if self.creating_count.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.creating_done.notify_waiters();
        }
        res
    }

    async fn enqueue(&self, snapshot: &Value) -> Result<ExportJobRecord, ServiceError> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(ServiceError::new(
                ServiceErrorKind::Unavailable,
                "export_stopping",
                "程序正在退出，请稍后导出",
            ));
        }
        self.initialize_work_dir_sync()?;
        {
            let mut guard = self.inner.lock().expect("export queue lock");
            let total =
                guard.queue.len() + guard.reservations + usize::from(guard.active.is_some());
            if total >= 8 {
                return Err(ServiceError::new(
                    ServiceErrorKind::Busy,
                    "export_busy",
                    "导出任务较多，请等待已有任务完成",
                ));
            }
            guard.reservations += 1;
        }

        let res = (|| -> Result<ExportJobRecord, ServiceError> {
            let id = Uuid::new_v4().to_string();
            let session_id = snapshot
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let title = snapshot
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("会话")
                .to_owned();
            let captured_at = snapshot
                .get("capturedAt")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let options = snapshot.get("options").cloned().unwrap_or(Value::Null);
            let date_str = options
                .get("date")
                .and_then(Value::as_str)
                .unwrap_or("1970-01-01");
            let display_dir = artifact_sessions_display_path(&self.root, date_str)?
                .to_string_lossy()
                .into_owned();

            let result = ExportJobRecord {
                id: id.clone(),
                session_id,
                title,
                captured_at,
                options,
                status: "queued".to_owned(),
                directory: Some(display_dir),
                files: Vec::new(),
                error: None,
            };
            let record = ExportRecord {
                result: result.clone(),
                stored: Vec::new(),
            };
            self.save_record_sync(&record)?;

            if self.closing.load(Ordering::SeqCst) {
                let _ = fs::remove_file(self.metadata.join(format!("{id}.json")));
                return Err(ServiceError::new(
                    ServiceErrorKind::Unavailable,
                    "export_stopping",
                    "程序正在退出，请稍后导出",
                ));
            }

            let work = Arc::new(WorkItem {
                id,
                snapshot: snapshot.clone(),
                state: Mutex::new((record, result.clone())),
            });

            {
                let mut guard = self.inner.lock().expect("export queue lock");
                guard.queue.push_back(work);
                self.revision.fetch_add(1, Ordering::SeqCst);
                self.ensure_pumping_locked(&mut guard);
            }
            Ok(result)
        })();

        self.inner.lock().expect("export queue lock").reservations -= 1;
        res
    }

    fn ensure_pumping_locked(&self, guard: &mut QueueState) {
        let is_running = guard.pumping.as_ref().is_some_and(|h| !h.is_finished());
        if is_running || guard.queue.is_empty() {
            return;
        }
        let store = self.clone();
        guard.pumping = Some(tokio::spawn(async move {
            store.pump_loop().await;
        }));
    }

    async fn pump_loop(&self) {
        loop {
            let next_work = {
                let mut guard = self.inner.lock().expect("export queue lock");
                let Some(work) = guard.queue.pop_front() else {
                    guard.pumping = None;
                    return;
                };
                let cancel = CancellationToken::new();
                guard.active = Some(work.clone());
                guard.active_cancel = Some(cancel.clone());
                guard.active_cancel_error = None;
                (work, cancel)
            };

            self.run_work(&next_work.0, next_work.1).await;

            {
                let mut guard = self.inner.lock().expect("export queue lock");
                guard.active = None;
                guard.active_cancel = None;
                guard.active_cancel_error = None;
                self.revision.fetch_add(1, Ordering::SeqCst);
                if guard.queue.is_empty() {
                    guard.pumping = None;
                    return;
                }
            }
        }
    }

    async fn run_work(&self, work: &Arc<WorkItem>, cancel: CancellationToken) {
        let staging = self.metadata.join("work").join(&work.id);
        let mut published: Vec<PathBuf> = Vec::new();

        let inner_for_timeout = self.inner.clone();
        let cancel_for_timeout = cancel.clone();
        let timeout_handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(120)).await;
            {
                let mut guard = inner_for_timeout.lock().expect("export queue lock");
                if guard.active_cancel_error.is_none() {
                    guard.active_cancel_error = Some(ServiceError::new(
                        ServiceErrorKind::Timeout,
                        "export_timeout",
                        "导出超时，请重试或缩短当前分支",
                    ));
                }
            }
            cancel_for_timeout.cancel();
        });

        let expected_format = work
            .snapshot
            .get("options")
            .and_then(|o| o.get("format"))
            .and_then(Value::as_str)
            .unwrap_or("png")
            .to_owned();
        let date_str = work
            .snapshot
            .get("options")
            .and_then(|o| o.get("date"))
            .and_then(Value::as_str)
            .unwrap_or("1970-01-01")
            .to_owned();

        struct StagedFile {
            file_path: PathBuf,
            name: String,
            hash: String,
            bytes: usize,
        }

        let outcome: Result<(), ServiceError> = async {
            if self.closing.load(Ordering::SeqCst) {
                return Err(ServiceError::new(
                    ServiceErrorKind::Unavailable,
                    "export_interrupted",
                    "导出已随程序退出取消，请重新导出",
                ));
            }

            {
                let mut guard = work.state.lock().expect("work state lock");
                guard.0.result.status = "rendering".to_owned();
            }
            self.commit_work_sync(work)?;

            fs::create_dir_all(&staging).map_err(|err| {
                let full = is_enospc(&err);
                ServiceError::new(
                    if full {
                        ServiceErrorKind::StorageFull
                    } else {
                        ServiceErrorKind::Internal
                    },
                    if full {
                        "storage_full"
                    } else {
                        "export_failed"
                    },
                    if full {
                        "工具包存储空间不足，请清理后重试"
                    } else {
                        "导出未完成，请检查浏览器和工具包写入权限后重试"
                    },
                )
            })?;

            let staged_files: Arc<Mutex<Vec<StagedFile>>> = Arc::new(Mutex::new(Vec::new()));
            let total_bytes: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
            let staging_dir = staging.clone();
            let expected_fmt = expected_format.clone();
            let cancel_for_emit = cancel.clone();
            let staged_ref = staged_files.clone();
            let bytes_ref = total_bytes.clone();

            let mut emit_cb = |ext: &'static str,
                               data: Vec<u8>|
             -> pi_agent::BoxFuture<'_, Result<(), ServiceError>> {
                let staging_dir = staging_dir.clone();
                let expected_fmt = expected_fmt.clone();
                let cancel_for_emit = cancel_for_emit.clone();
                let staged_ref = staged_ref.clone();
                let bytes_ref = bytes_ref.clone();
                Box::pin(async move {
                    if cancel_for_emit.is_cancelled() {
                        return Err(ServiceError::new(
                            ServiceErrorKind::Unavailable,
                            "export_interrupted",
                            "导出已取消",
                        ));
                    }
                    let new_total = bytes_ref
                        .fetch_add(data.len(), Ordering::SeqCst)
                        .saturating_add(data.len());
                    let max_files = if ext == "html" { 1 } else { 64 };
                    let current_len = staged_ref.lock().expect("staged lock").len();
                    if ext != expected_fmt
                        || current_len >= max_files
                        || new_total > 200 * 1024 * 1024
                        || data.is_empty()
                    {
                        return Err(ServiceError::new(
                            ServiceErrorKind::TooLarge,
                            "export_too_large",
                            "产物超过大小限制，请缩短当前分支后重试",
                        ));
                    }
                    let name = format!("{:02}.{ext}", current_len + 1);
                    let file_path = staging_dir.join(&name);
                    let mut f = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&file_path)
                        .map_err(|err| {
                            let full = is_enospc(&err);
                            ServiceError::new(
                                if full {
                                    ServiceErrorKind::StorageFull
                                } else {
                                    ServiceErrorKind::Internal
                                },
                                if full {
                                    "storage_full"
                                } else {
                                    "export_failed"
                                },
                                if full {
                                    "工具包存储空间不足，请清理后重试"
                                } else {
                                    "导出未完成，请检查浏览器和工具包写入权限后重试"
                                },
                            )
                        })?;
                    f.write_all(&data).map_err(|err| {
                        let full = is_enospc(&err);
                        ServiceError::new(
                            if full {
                                ServiceErrorKind::StorageFull
                            } else {
                                ServiceErrorKind::Internal
                            },
                            if full {
                                "storage_full"
                            } else {
                                "export_failed"
                            },
                            if full {
                                "工具包存储空间不足，请清理后重试"
                            } else {
                                "导出未完成，请检查浏览器和工具包写入权限后重试"
                            },
                        )
                    })?;
                    let hash = format!("{:x}", Sha256::digest(&data));
                    staged_ref.lock().expect("staged lock").push(StagedFile {
                        file_path,
                        name,
                        hash,
                        bytes: data.len(),
                    });
                    Ok(())
                })
            };

            self.renderer
                .render(&work.snapshot, &mut emit_cb, cancel.clone())
                .await?;

            if cancel.is_cancelled() {
                return Err(ServiceError::new(
                    ServiceErrorKind::Unavailable,
                    "export_interrupted",
                    "导出已取消",
                ));
            }

            let mut files = std::mem::take(&mut *staged_files.lock().expect("staged lock"));
            if files.is_empty() {
                return Err(ServiceError::new(
                    ServiceErrorKind::Internal,
                    "export_failed",
                    "导出未完成，请检查浏览器和工具包写入权限后重试",
                ));
            }

            let target_dir = ensure_artifact_sessions_dir(&self.root, &date_str)?;
            let base = export_base_stem(&work.snapshot);

            for group in 1usize.. {
                if cancel.is_cancelled() {
                    return Err(ServiceError::new(
                        ServiceErrorKind::Unavailable,
                        "export_interrupted",
                        "导出已取消",
                    ));
                }
                let stem = if group == 1 {
                    base.clone()
                } else {
                    format!("{base}-{group}")
                };
                let mut collision = false;
                for (index, item) in files.iter_mut().enumerate() {
                    if cancel.is_cancelled() {
                        return Err(ServiceError::new(
                            ServiceErrorKind::Unavailable,
                            "export_interrupted",
                            "导出已取消",
                        ));
                    }
                    let page_suffix = if expected_format == "png" {
                        format!("-{:02}", index + 1)
                    } else {
                        String::new()
                    };
                    item.name = format!("{stem}{page_suffix}.{expected_format}");
                    let target = target_dir.join(&item.name);
                    match copy_file_exclusive(&item.file_path, &target) {
                        Ok(()) => published.push(target),
                        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                            collision = true;
                            break;
                        }
                        Err(err) => {
                            let full = is_enospc(&err);
                            return Err(ServiceError::new(
                                if full {
                                    ServiceErrorKind::StorageFull
                                } else {
                                    ServiceErrorKind::Internal
                                },
                                if full {
                                    "storage_full"
                                } else {
                                    "export_failed"
                                },
                                if full {
                                    "工具包存储空间不足，请清理后重试"
                                } else {
                                    "导出未完成，请检查浏览器和工具包写入权限后重试"
                                },
                            ));
                        }
                    }
                }
                if !collision {
                    break;
                }
                for created in published.drain(..) {
                    let _ = fs::remove_file(created);
                }
            }

            if cancel.is_cancelled() {
                return Err(ServiceError::new(
                    ServiceErrorKind::Unavailable,
                    "export_interrupted",
                    "导出已取消",
                ));
            }

            {
                let mut guard = work.state.lock().expect("work state lock");
                guard.0.stored = files
                    .iter()
                    .map(|f| StoredFileRecord {
                        name: f.name.clone(),
                        hash: f.hash.clone(),
                    })
                    .collect();
                guard.0.result.files = files
                    .iter()
                    .enumerate()
                    .map(|(index, f)| ExportFileEntry {
                        name: f.name.clone(),
                        bytes: f.bytes,
                        url: format!("/api/exports/{}/files/{index}", work.id),
                    })
                    .collect();
                guard.0.result.status = "completed".to_owned();
            }
            self.commit_work_sync(work)?;
            Ok(())
        }
        .await;

        timeout_handle.abort();

        if let Err(err) = outcome {
            for created in published.drain(..) {
                let _ = fs::remove_file(created);
            }
            let override_err = self
                .inner
                .lock()
                .expect("export queue lock")
                .active_cancel_error
                .clone();
            let final_err = override_err.unwrap_or_else(|| {
                if cancel.is_cancelled()
                    && err.code == "export_interrupted"
                    && self.closing.load(Ordering::SeqCst)
                {
                    ServiceError::new(
                        ServiceErrorKind::Unavailable,
                        "export_interrupted",
                        "导出已随程序退出取消，请重新导出",
                    )
                } else {
                    err
                }
            });
            let record_to_save = {
                let mut guard = work.state.lock().expect("work state lock");
                guard.0.result.status = "failed".to_owned();
                guard.0.result.files.clear();
                guard.0.stored.clear();
                guard.0.result.error = Some(json!({
                    "code": final_err.code,
                    "message": final_err.message,
                }));
                guard.1 = guard.0.result.clone();
                guard.0.clone()
            };
            let _ = self.save_record_sync(&record_to_save);
        }

        let _ = fs::remove_dir_all(&staging);
    }

    fn live_record(&self, id: &str) -> Option<ExportRecord> {
        let guard = self.inner.lock().expect("export queue lock");
        if let Some(ref active) = guard.active
            && active.id == id
        {
            return Some(active.visible_record());
        }
        guard
            .queue
            .iter()
            .find(|item| item.id == id)
            .map(|item| item.visible_record())
    }

    fn read_record_sync(&self, id: &str) -> Result<ExportRecord, ServiceError> {
        if !is_valid_export_id(id) {
            return Err(ServiceError::new(
                ServiceErrorKind::NotFound,
                "export_not_found",
                "导出记录不存在",
            ));
        }

        self.initialize_work_dir_sync()?;
        loop {
            if let Some(live) = self.live_record(id) {
                return Ok(live);
            }
            let rev = self.revision.load(Ordering::SeqCst);
            let file_path = self.metadata.join(format!("{id}.json"));
            let bytes = match fs::read(&file_path) {
                Ok(b) => b,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    return Err(ServiceError::new(
                        ServiceErrorKind::NotFound,
                        "export_not_found",
                        "导出记录不存在",
                    ));
                }
                Err(_) => {
                    return Err(ServiceError::new(
                        ServiceErrorKind::Unavailable,
                        "export_record_unavailable",
                        "导出记录无法读取，请检查工具包所在介质",
                    ));
                }
            };

            if let Some(live) = self.live_record(id) {
                return Ok(live);
            }
            if self.revision.load(Ordering::SeqCst) != rev {
                continue;
            }

            let mut record: ExportRecord = serde_json::from_slice(&bytes).map_err(|_| {
                ServiceError::new(
                    ServiceErrorKind::Unavailable,
                    "export_record_unavailable",
                    "导出记录无法读取，请检查工具包所在介质",
                )
            })?;

            let valid_status = matches!(
                record.result.status.as_str(),
                "queued" | "rendering" | "completed" | "failed"
            );
            if record.result.id != id || !valid_status {
                return Err(ServiceError::new(
                    ServiceErrorKind::Unavailable,
                    "export_record_unavailable",
                    "导出记录无法读取，请检查工具包所在介质",
                ));
            }

            let Some(opts_obj) = record.result.options.as_object() else {
                return Err(ServiceError::new(
                    ServiceErrorKind::Unavailable,
                    "export_record_unavailable",
                    "导出记录无法读取，请检查工具包所在介质",
                ));
            };
            let date_str = opts_obj
                .get("date")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ServiceError::new(
                        ServiceErrorKind::Unavailable,
                        "export_record_unavailable",
                        "导出记录无法读取，请检查工具包所在介质",
                    )
                })?;
            validate_artifact_date(date_str).map_err(|_| {
                ServiceError::new(
                    ServiceErrorKind::Unavailable,
                    "export_record_unavailable",
                    "导出记录无法读取，请检查工具包所在介质",
                )
            })?;
            let mut stripped_opts = opts_obj.clone();
            stripped_opts.remove("date");
            parse_export_options(&Value::Object(stripped_opts)).map_err(|_| {
                ServiceError::new(
                    ServiceErrorKind::Unavailable,
                    "export_record_unavailable",
                    "导出记录无法读取，请检查工具包所在介质",
                )
            })?;

            if matches!(record.result.status.as_str(), "queued" | "rendering") {
                record.result.status = "failed".to_owned();
                record.result.error = Some(json!({
                    "code": "export_interrupted",
                    "message": "上次导出未完成，请重新导出",
                }));
                self.save_record_sync(&record)?;
            }

            return Ok(record);
        }
    }

    pub async fn get(&self, id: &str) -> Result<ExportJobRecord, ServiceError> {
        Ok(self.read_record_sync(id)?.result)
    }

    pub async fn list(&self, session_id: &str) -> Result<Vec<ExportJobRecord>, ServiceError> {
        self.initialize_work_dir_sync()?;
        let entries = match fs::read_dir(&self.metadata) {
            Ok(rd) => rd,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => {
                return Err(ServiceError::new(
                    ServiceErrorKind::Unavailable,
                    "export_record_unavailable",
                    "导出记录无法读取，请检查工具包所在介质",
                ));
            }
        };

        let mut results = Vec::new();
        for entry in entries.filter_map(Result::ok) {
            let file_name = entry.file_name();
            let Some(name_str) = file_name.to_str() else {
                continue;
            };
            let Some(id_part) = name_str.strip_suffix(".json") else {
                continue;
            };
            if !is_valid_export_id(id_part) {
                continue;
            }
            if let Ok(rec) = self.read_record_sync(id_part)
                && rec.result.session_id == session_id
            {
                results.push(rec.result);
            }
        }
        results.sort_by(|a, b| b.captured_at.cmp(&a.captured_at));
        results.truncate(50);
        Ok(results)
    }

    pub async fn directory(&self, id: &str) -> Result<PathBuf, ServiceError> {
        let record = self.read_record_sync(id)?;
        if record.result.status != "completed" {
            return Err(ServiceError::new(
                ServiceErrorKind::Conflict,
                "export_not_completed",
                "导出尚未保存完成，请完成后打开目录",
            ));
        }
        let date_str = record
            .result
            .options
            .get("date")
            .and_then(Value::as_str)
            .unwrap_or("");
        existing_artifact_sessions_dir(&self.root, date_str)
    }

    pub async fn file(&self, id: &str, index: usize) -> Result<ExportDownloadedFile, ServiceError> {
        let record = self.read_record_sync(id)?;
        let Some(stored_entry) = record.stored.get(index) else {
            return Err(ServiceError::new(
                ServiceErrorKind::NotFound,
                "export_file_missing",
                "导出文件不存在",
            ));
        };
        let is_single_filename = Path::new(&stored_entry.name)
            .file_name()
            .and_then(|n| n.to_str())
            == Some(stored_entry.name.as_str())
            && !stored_entry.name.contains('/')
            && !stored_entry.name.contains('\\');
        if record.result.status != "completed" || !is_single_filename {
            return Err(ServiceError::new(
                ServiceErrorKind::NotFound,
                "export_file_missing",
                "导出文件不存在",
            ));
        }

        let date_str = record
            .result
            .options
            .get("date")
            .and_then(Value::as_str)
            .unwrap_or("");
        let directory = existing_artifact_sessions_dir(&self.root, date_str).map_err(|_| {
            ServiceError::new(
                ServiceErrorKind::NotFound,
                "export_file_missing",
                "导出文件已移走或无法读取，请重新导出",
            )
        })?;
        let target = fs::canonicalize(directory.join(&stored_entry.name))
            .map(|p| strip_windows_verbatim_prefix(&p))
            .map_err(|_| {
                ServiceError::new(
                    ServiceErrorKind::NotFound,
                    "export_file_missing",
                    "导出文件已移走或无法读取，请重新导出",
                )
            })?;
        if !target.starts_with(&directory) || target == directory {
            return Err(ServiceError::new(
                ServiceErrorKind::NotFound,
                "export_file_missing",
                "导出文件已移走或无法读取，请重新导出",
            ));
        }

        let data = fs::read(&target).map_err(|_| {
            ServiceError::new(
                ServiceErrorKind::NotFound,
                "export_file_missing",
                "导出文件已移走或无法读取，请重新导出",
            )
        })?;
        let actual_hash = format!("{:x}", Sha256::digest(&data));
        if actual_hash != stored_entry.hash {
            return Err(ServiceError::new(
                ServiceErrorKind::Conflict,
                "export_file_changed",
                "导出文件已被修改，请重新导出后分享",
            ));
        }

        let format = record
            .result
            .options
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("png");
        let content_type = if format == "html" {
            "text/html; charset=utf-8"
        } else {
            "image/png"
        };

        Ok(ExportDownloadedFile {
            name: stored_entry.name.clone(),
            data,
            content_type,
        })
    }

    pub async fn close(&self) {
        self.closing.store(true, Ordering::SeqCst);
        {
            let mut guard = self.inner.lock().expect("export queue lock");
            if guard.active_cancel_error.is_none() {
                guard.active_cancel_error = Some(ServiceError::new(
                    ServiceErrorKind::Unavailable,
                    "export_interrupted",
                    "导出已随程序退出取消，请重新导出",
                ));
            }
            if let Some(ref cancel) = guard.active_cancel {
                cancel.cancel();
            }
        }

        let mut close_guard = self.close_lock.lock().await;
        if *close_guard {
            return;
        }

        while self.creating_count.load(Ordering::SeqCst) > 0 {
            let notified = self.creating_done.notified();
            if self.creating_count.load(Ordering::SeqCst) == 0 {
                break;
            }
            notified.await;
        }

        let pump_handle = self.inner.lock().expect("export queue lock").pumping.take();
        if let Some(h) = pump_handle {
            let _ = h.await;
        }

        self.renderer.close().await;
        let _ = self.clear_work_dir_sync();
        *close_guard = true;
    }
}

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cst_pilot::SKILL_NAMES;
use pi_agent::BoxFuture;
use pi_ai::now_ms;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex as AsyncMutex, Notify};

use crate::locks::{StorageError, atomic_write_json};
use crate::session::attachments::{ImageRef, is_valid_session_id_key};

pub const MAX_MESSAGE_TEXT_CHARS: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Delivery {
    Queue,
    Steer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemDelivery {
    Queue,
    Steer,
    Direct,
}

impl From<Delivery> for ItemDelivery {
    fn from(value: Delivery) -> Self {
        match value {
            Delivery::Queue => Self::Queue,
            Delivery::Steer => Self::Steer,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemStatus {
    Pending,
    Delivering,
    Delivered,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxItem {
    pub id: String,
    pub sequence: u64,
    pub text: String,
    pub delivery: ItemDelivery,
    pub accepted_text: String,
    pub accepted_delivery: Delivery,
    #[serde(default)]
    pub images: Vec<ImageRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    pub status: ItemStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uncertain: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub accepted_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxSnapshot {
    pub version: u64,
    pub next_sequence: u64,
    pub paused: bool,
    pub items: Vec<InboxItem>,
}

impl Default for InboxSnapshot {
    fn default() -> Self {
        Self {
            version: 0,
            next_sequence: 1,
            paused: false,
            items: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct QueueEdit {
    pub text: Option<String>,
    pub delivery: Option<Delivery>,
    pub remove: bool,
    pub retry: bool,
}

#[derive(Debug, Clone)]
pub enum InboxError {
    InvalidInput(String),
    Conflict(String),
    Corrupted(String),
    Storage(StorageError),
}

impl std::fmt::Display for InboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput(msg) | Self::Conflict(msg) | Self::Corrupted(msg) => {
                f.write_str(msg)
            }
            Self::Storage(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for InboxError {}

pub trait InboxExecutor: Send + Sync {
    fn is_busy(&self) -> bool;
    fn prompt(
        &self,
        text: String,
        images: Vec<ImageRef>,
        skill: Option<String>,
        message_id: String,
    ) -> BoxFuture<'static, Result<(), String>>;
    fn steer(
        &self,
        text: String,
        images: Vec<ImageRef>,
        skill: Option<String>,
        message_id: String,
    ) -> BoxFuture<'static, Result<(), String>>;
    fn was_accepted(&self, id: &str) -> bool {
        let _ = id;
        false
    }
}

struct DispatchCoord {
    dispatching: bool,
    wake_requested: bool,
}

type InboxChangeCallback = Arc<dyn Fn(InboxSnapshot) + Send + Sync>;

/// Per-session durable input inbox (`agent/home/web-inbox/<sessionId>.json`).
/// Handles `direct / queue / steer` scheduling, `prompting` attachment preparation ordering,
/// versioned queue edits/reordering, and crash recovery (`delivering -> uncertain: true` without replay).
#[derive(Clone)]
pub struct SessionInbox {
    session_id: String,
    file: PathBuf,
    serial: Arc<AsyncMutex<Option<InboxSnapshot>>>,
    executor: Arc<dyn InboxExecutor>,
    on_change: Option<InboxChangeCallback>,
    closed: Arc<AtomicBool>,
    prompting: Arc<AtomicBool>,
    coord: Arc<Mutex<DispatchCoord>>,
    in_flight_count: Arc<AtomicUsize>,
    idle_notify: Arc<Notify>,
}

impl SessionInbox {
    pub fn new(
        root: impl AsRef<Path>,
        session_id: impl Into<String>,
        executor: Arc<dyn InboxExecutor>,
        on_change: Option<InboxChangeCallback>,
    ) -> Result<Self, InboxError> {
        let session_id = session_id.into();
        if !is_valid_session_id_key(&session_id) {
            return Err(InboxError::InvalidInput("会话 ID 无效".to_owned()));
        }
        let file = root.as_ref().join(format!("{session_id}.json"));
        Ok(Self {
            session_id,
            file,
            serial: Arc::new(AsyncMutex::new(None)),
            executor,
            on_change,
            closed: Arc::new(AtomicBool::new(false)),
            prompting: Arc::new(AtomicBool::new(false)),
            coord: Arc::new(Mutex::new(DispatchCoord {
                dispatching: false,
                wake_requested: false,
            })),
            in_flight_count: Arc::new(AtomicUsize::new(0)),
            idle_notify: Arc::new(Notify::new()),
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn file_path(&self) -> &Path {
        &self.file
    }

    async fn ensure_loaded<'a>(
        &self,
        guard: &'a mut Option<InboxSnapshot>,
    ) -> Result<&'a mut InboxSnapshot, InboxError> {
        if guard.is_none() {
            let path = self.file.clone();
            let loaded = tokio::task::spawn_blocking(move || match fs::read(&path) {
                Ok(bytes) => {
                    let mut parsed: InboxSnapshot = serde_json::from_slice(&bytes)
                        .map_err(|_| InboxError::Corrupted("输入收件箱损坏".to_owned()))?;
                    if !is_valid_snapshot(&parsed) {
                        return Err(InboxError::Corrupted("输入收件箱损坏".to_owned()));
                    }
                    // Crash recovery: leftover `delivering` items are marked `uncertain: true` and never replayed.
                    for item in &mut parsed.items {
                        if item.status == ItemStatus::Delivering {
                            item.uncertain = Some(true);
                        }
                    }
                    Ok(parsed)
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(InboxSnapshot::default()),
                Err(err) => Err(InboxError::Storage(StorageError::from_io(
                    &err,
                    "读取输入收件箱",
                ))),
            })
            .await
            .map_err(|_| InboxError::Corrupted("读取输入收件箱任务异常结束".to_owned()))??;
            *guard = Some(loaded);
        }
        Ok(guard.as_mut().expect("initialized"))
    }

    async fn save_locked(
        &self,
        slot: &mut Option<InboxSnapshot>,
        next: InboxSnapshot,
    ) -> Result<(), InboxError> {
        atomic_write_json(&self.file, &next, "保存输入收件箱")
            .await
            .map_err(InboxError::Storage)?;
        *slot = Some(next.clone());
        if let Some(cb) = &self.on_change {
            cb(next);
        }
        Ok(())
    }

    pub async fn snapshot(&self) -> Result<InboxSnapshot, InboxError> {
        let mut guard = self.serial.lock().await;
        let current = self.ensure_loaded(&mut guard).await?;
        Ok(current.clone())
    }

    /// Returns true if the inbox has any `Pending` or `Delivering` items or active delivery tasks.
    pub async fn has_active_or_pending(&self) -> bool {
        if self.in_flight_count.load(Ordering::SeqCst) > 0 || self.prompting.load(Ordering::SeqCst)
        {
            return true;
        }
        let mut guard = self.serial.lock().await;
        let Ok(current) = self.ensure_loaded(&mut guard).await else {
            return false;
        };
        current.items.iter().any(|item| {
            matches!(item.status, ItemStatus::Pending | ItemStatus::Delivering)
                && item.uncertain != Some(true)
        })
    }

    pub async fn has_delivering(&self) -> Result<bool, InboxError> {
        let snap = self.snapshot().await?;
        Ok(snap
            .items
            .iter()
            .any(|item| item.status == ItemStatus::Delivering))
    }

    pub async fn accept(
        &self,
        id: &str,
        text: &str,
        delivery: Delivery,
        images: Vec<ImageRef>,
        skill: Option<String>,
    ) -> Result<InboxItem, InboxError> {
        if let Some(ref s) = skill
            && !SKILL_NAMES.contains(&s.as_str())
        {
            return Err(InboxError::InvalidInput("技能名称无效".to_owned()));
        }
        let char_count = text.chars().count();
        if !is_valid_session_id_key(id)
            || (text.trim().is_empty() && images.is_empty() && skill.is_none())
            || char_count > MAX_MESSAGE_TEXT_CHARS
        {
            return Err(InboxError::InvalidInput("消息格式无效".to_owned()));
        }

        let item = {
            let mut guard = self.serial.lock().await;
            let current = self.ensure_loaded(&mut guard).await?.clone();
            if let Some(existing) = current.items.iter().find(|entry| entry.id == id) {
                if existing.accepted_text != text
                    || existing.accepted_delivery != delivery
                    || existing.images != images
                    || existing.skill != skill
                {
                    return Err(InboxError::Conflict("消息 ID 已用于其他内容".to_owned()));
                }
                existing.clone()
            } else {
                let direct = delivery == Delivery::Queue
                    && !current.paused
                    && !self.prompting.load(Ordering::SeqCst)
                    && !self.executor.is_busy()
                    && !current.items.iter().any(|entry| {
                        matches!(entry.status, ItemStatus::Pending | ItemStatus::Delivering)
                    });
                let added = InboxItem {
                    id: id.to_owned(),
                    sequence: current.next_sequence,
                    text: text.to_owned(),
                    delivery: if direct {
                        ItemDelivery::Direct
                    } else {
                        ItemDelivery::from(delivery)
                    },
                    accepted_text: text.to_owned(),
                    accepted_delivery: delivery,
                    images,
                    skill,
                    status: ItemStatus::Pending,
                    uncertain: None,
                    error: None,
                    accepted_at: now_ms(),
                };
                let mut items = current.items;
                items.push(added.clone());
                let next = InboxSnapshot {
                    version: current.version + 1,
                    next_sequence: current.next_sequence + 1,
                    paused: false,
                    items,
                };
                self.save_locked(&mut guard, next).await?;
                added
            }
        };

        self.schedule();
        Ok(item)
    }

    pub async fn pause(&self) -> Result<(), InboxError> {
        let mut guard = self.serial.lock().await;
        let current = self.ensure_loaded(&mut guard).await?.clone();
        let next = InboxSnapshot {
            version: current.version + 1,
            next_sequence: current.next_sequence,
            paused: true,
            items: current.items,
        };
        self.save_locked(&mut guard, next).await
    }

    pub async fn change(
        &self,
        id: &str,
        version: u64,
        edit: QueueEdit,
    ) -> Result<InboxSnapshot, InboxError> {
        let result = {
            let mut guard = self.serial.lock().await;
            let current = self.ensure_loaded(&mut guard).await?.clone();
            if current.version != version {
                return Err(InboxError::Conflict("队列已更新，请重新读取".to_owned()));
            }
            let Some(target) = current.items.iter().find(|item| item.id == id) else {
                return Err(InboxError::Conflict("条目已投递或不存在".to_owned()));
            };
            if !matches!(target.status, ItemStatus::Pending | ItemStatus::Failed) {
                return Err(InboxError::Conflict("条目已投递或不存在".to_owned()));
            }
            if edit.retry && target.status != ItemStatus::Failed {
                return Err(InboxError::Conflict("仅失败条目可以重试".to_owned()));
            }
            if edit.delivery.is_some() && target.status != ItemStatus::Pending {
                return Err(InboxError::Conflict("失败条目请先重试".to_owned()));
            }
            if edit.retry && edit.remove {
                return Err(InboxError::InvalidInput("操作无效".to_owned()));
            }
            if let Some(ref new_text) = edit.text
                && ((new_text.trim().is_empty()
                    && target.images.is_empty()
                    && target.skill.is_none())
                    || new_text.chars().count() > MAX_MESSAGE_TEXT_CHARS)
            {
                return Err(InboxError::InvalidInput("消息格式无效".to_owned()));
            }

            let items = current
                .items
                .into_iter()
                .map(|mut item| {
                    if item.id == id {
                        if let Some(ref t) = edit.text {
                            item.text = t.clone();
                        }
                        if edit.retry && item.delivery == ItemDelivery::Direct {
                            item.delivery = ItemDelivery::Queue;
                        } else if let Some(d) = edit.delivery {
                            item.delivery = ItemDelivery::from(d);
                        }
                        if edit.remove {
                            item.status = ItemStatus::Cancelled;
                        } else if edit.retry {
                            item.status = ItemStatus::Pending;
                            item.error = None;
                        }
                    }
                    item
                })
                .collect();

            let next = InboxSnapshot {
                version: version + 1,
                next_sequence: current.next_sequence,
                paused: if edit.retry { false } else { current.paused },
                items,
            };
            self.save_locked(&mut guard, next.clone()).await?;
            next
        };
        self.schedule();
        Ok(result)
    }

    pub async fn reorder(&self, ids: &[String], version: u64) -> Result<InboxSnapshot, InboxError> {
        let mut guard = self.serial.lock().await;
        let current = self.ensure_loaded(&mut guard).await?.clone();
        if current.version != version {
            return Err(InboxError::Conflict("队列已更新，请重新读取".to_owned()));
        }
        let pending: Vec<InboxItem> = current
            .items
            .iter()
            .filter(|item| {
                item.status == ItemStatus::Pending && item.delivery == ItemDelivery::Queue
            })
            .cloned()
            .collect();
        let unique_ids: HashSet<&String> = ids.iter().collect();
        if ids.len() != pending.len()
            || unique_ids.len() != ids.len()
            || pending.iter().any(|item| !unique_ids.contains(&item.id))
        {
            return Err(InboxError::Conflict("排序条目与当前队列不一致".to_owned()));
        }

        let ordered: Vec<InboxItem> = ids
            .iter()
            .filter_map(|id| pending.iter().find(|item| &item.id == id).cloned())
            .collect();
        let slot_ids: HashSet<String> = pending.into_iter().map(|i| i.id).collect();
        let mut ordered_iter = ordered.into_iter();
        let items = current
            .items
            .into_iter()
            .map(|item| {
                if slot_ids.contains(&item.id) {
                    ordered_iter.next().unwrap_or(item)
                } else {
                    item
                }
            })
            .collect();

        let next = InboxSnapshot {
            version: version + 1,
            next_sequence: current.next_sequence,
            paused: current.paused,
            items,
        };
        self.save_locked(&mut guard, next.clone()).await?;
        Ok(next)
    }

    pub async fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        loop {
            let notified = self.idle_notify.notified();
            let dispatching = self.coord.lock().expect("dispatch coord").dispatching;
            if !dispatching && self.in_flight_count.load(Ordering::SeqCst) == 0 {
                break;
            }
            notified.await;
        }
        let _ = self.serial.lock().await;
    }

    pub fn wake(&self) {
        self.schedule();
    }

    fn schedule(&self) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        {
            let mut coord = self.coord.lock().expect("dispatch coord");
            if coord.dispatching {
                coord.wake_requested = true;
                return;
            }
            coord.dispatching = true;
        }

        let inbox = self.clone();
        tokio::spawn(async move {
            loop {
                inbox.drain().await;
                let mut coord = inbox.coord.lock().expect("dispatch coord");
                if coord.wake_requested && !inbox.closed.load(Ordering::SeqCst) {
                    coord.wake_requested = false;
                    continue;
                }
                coord.dispatching = false;
                break;
            }
            inbox.idle_notify.notify_waiters();
        });
    }

    async fn drain(&self) {
        while !self.closed.load(Ordering::SeqCst) {
            let next_step = {
                let mut guard = self.serial.lock().await;
                let Ok(current_ref) = self.ensure_loaded(&mut guard).await else {
                    return;
                };
                let current = current_ref.clone();
                if current.paused {
                    None
                } else {
                    let failed = current
                        .items
                        .iter()
                        .any(|item| item.status == ItemStatus::Failed);
                    let session_busy = self.executor.is_busy();
                    let is_prompting = self.prompting.load(Ordering::SeqCst);
                    // Wait for the initial prompt's attachment preparation before allowing steer to overtake it.
                    if is_prompting && !session_busy {
                        None
                    } else {
                        let busy = is_prompting || session_busy;
                        if busy
                            && current.items.iter().any(|item| {
                                item.status == ItemStatus::Pending
                                    && item.delivery == ItemDelivery::Direct
                            })
                        {
                            let items = current
                                .items
                                .into_iter()
                                .map(|mut item| {
                                    if item.status == ItemStatus::Pending
                                        && item.delivery == ItemDelivery::Direct
                                    {
                                        item.delivery = ItemDelivery::Queue;
                                    }
                                    item
                                })
                                .collect();
                            let updated = InboxSnapshot {
                                version: current.version + 1,
                                next_sequence: current.next_sequence,
                                paused: current.paused,
                                items,
                            };
                            if self.save_locked(&mut guard, updated).await.is_err() {
                                return;
                            }
                            self.coord.lock().expect("dispatch coord").wake_requested = true;
                            None
                        } else {
                            let candidate = current
                                .items
                                .iter()
                                .find(|item| {
                                    item.status == ItemStatus::Pending
                                        && if failed {
                                            item.delivery == ItemDelivery::Direct && !busy
                                        } else {
                                            item.delivery == ItemDelivery::Steer || !busy
                                        }
                                })
                                .cloned();
                            match candidate {
                                Some(mut item) => {
                                    item.status = ItemStatus::Delivering;
                                    let items = current
                                        .items
                                        .into_iter()
                                        .map(|entry| {
                                            if entry.id == item.id {
                                                item.clone()
                                            } else {
                                                entry
                                            }
                                        })
                                        .collect();
                                    let updated = InboxSnapshot {
                                        version: current.version + 1,
                                        next_sequence: current.next_sequence,
                                        paused: current.paused,
                                        items,
                                    };
                                    if self.save_locked(&mut guard, updated).await.is_err() {
                                        return;
                                    }
                                    Some((item, busy))
                                }
                                None => None,
                            }
                        }
                    }
                }
            };

            let Some((item, busy)) = next_step else {
                return;
            };

            let steering = busy && item.delivery == ItemDelivery::Steer;
            if !steering {
                self.prompting.store(true, Ordering::SeqCst);
            }
            self.in_flight_count.fetch_add(1, Ordering::SeqCst);

            let inbox = self.clone();
            let task = tokio::spawn(async move {
                let delivered = inbox.deliver(item, steering).await;
                if !steering {
                    inbox.prompting.store(false, Ordering::SeqCst);
                }
                inbox.in_flight_count.fetch_sub(1, Ordering::SeqCst);
                inbox.idle_notify.notify_waiters();
                if delivered {
                    inbox.schedule();
                }
                delivered
            });

            if steering {
                if !task.await.unwrap_or(false) {
                    return;
                }
            } else {
                // Allow the drain loop to re-check for incoming `steer` items while `prompt` runs in background.
                tokio::task::yield_now().await;
            }
        }
    }

    async fn deliver(&self, item: InboxItem, steering: bool) -> bool {
        let mut completed = false;
        let exec_res = if steering {
            self.executor
                .steer(
                    item.text.clone(),
                    item.images.clone(),
                    item.skill.clone(),
                    item.id.clone(),
                )
                .await
        } else {
            self.executor
                .prompt(
                    item.text.clone(),
                    item.images.clone(),
                    item.skill.clone(),
                    item.id.clone(),
                )
                .await
        };

        if exec_res.is_ok() {
            completed = true;
            let mut guard = self.serial.lock().await;
            if let Ok(current_ref) = self.ensure_loaded(&mut guard).await {
                let current = current_ref.clone();
                let items = current
                    .items
                    .into_iter()
                    .map(|mut entry| {
                        if entry.id == item.id {
                            entry.status = ItemStatus::Delivered;
                        }
                        entry
                    })
                    .collect();
                let updated = InboxSnapshot {
                    version: current.version + 1,
                    next_sequence: current.next_sequence,
                    paused: current.paused,
                    items,
                };
                if self.save_locked(&mut guard, updated).await.is_ok() {
                    return true;
                }
            }
        }

        let accepted = completed || self.executor.was_accepted(&item.id);
        let error_msg: String = exec_res
            .err()
            .unwrap_or_else(|| "消息状态保存失败，请刷新后核对。".to_owned())
            .chars()
            .take(500)
            .collect();

        let mut guard = self.serial.lock().await;
        if let Ok(current_ref) = self.ensure_loaded(&mut guard).await {
            let current = current_ref.clone();
            let items = current
                .items
                .into_iter()
                .map(|mut entry| {
                    if entry.id == item.id {
                        entry.status = if accepted {
                            ItemStatus::Delivering
                        } else {
                            ItemStatus::Failed
                        };
                        entry.uncertain = if accepted { Some(true) } else { None };
                        entry.error = Some(error_msg.clone());
                    }
                    entry
                })
                .collect();
            let updated = InboxSnapshot {
                version: current.version + 1,
                next_sequence: current.next_sequence,
                paused: if accepted { current.paused } else { true },
                items,
            };
            if self.save_locked(&mut guard, updated.clone()).await.is_err() && accepted {
                // Keep in-memory uncertain state visible even if disk write failed.
                *guard = Some(updated.clone());
                if let Some(cb) = &self.on_change {
                    cb(updated);
                }
            }
        }
        false
    }
}

fn is_valid_snapshot(snapshot: &InboxSnapshot) -> bool {
    snapshot
        .items
        .iter()
        .all(|item| is_valid_session_id_key(&item.id))
}

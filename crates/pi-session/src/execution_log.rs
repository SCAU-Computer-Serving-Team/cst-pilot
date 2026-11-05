use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pi_agent::{AgentMessage, ToolReplayPolicy};
use pi_ai::{ContentBlock, ImageContent, ToolResultMessage, now_ms};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::session_manager::{SessionEntry, SessionManager};
use crate::session_store::{SessionStoreError, atomic_replace_file_sync};

pub const INPUT_CORRELATION_CUSTOM_TYPE: &str = "pi.input-correlation";
pub const CST_WEB_INPUT_CUSTOM_TYPE: &str = "cst-web-input-message";
pub const WORK_DURATION_CUSTOM_TYPE: &str = "cst-web-work-duration";
pub const RUN_INTERRUPTED_CUSTOM_TYPE: &str = "pi.run-interrupted";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptDisposition {
    Started,
    Steered,
    FollowedUp,
    HandledByExtension,
}

/// 客户端提交输入的生命周期状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum InputDeliveryStatus {
    /// 已通过预检并写入持久收件箱，等待调度投递。
    Queued,
    /// 已开始向 `AgentSession` 投递，若此时进程崩溃则投递结果未知。
    Delivering,
    /// 已确认为会话中的正式条目（或已被扩展 `input` 钩子 `Handled` 消化）。
    Accepted {
        disposition: PromptDisposition,
        #[serde(skip_serializing_if = "Option::is_none")]
        user_entry_id: Option<String>,
    },
    /// 投递前明确失败，可安全原样重试或编辑。
    Failed { error: String },
    /// 上次进程在 `Delivering` 阶段异常中断，投递结果待核对，禁止自动重发。
    Uncertain { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputCorrelationRecord {
    pub client_message_id: String,
    pub queue_item_id: String,
    pub text: String,
    #[serde(default)]
    pub images: Vec<ImageContent>,
    pub status: InputDeliveryStatus,
    pub uncertain: bool,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InFlightToolRecord {
    pub tool_call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    pub tool_name: String,
    #[serde(default = "default_replay_never")]
    pub replay_policy: ToolReplayPolicy,
    pub started_at_ms: u64,
}

fn default_replay_never() -> ToolReplayPolicy {
    ToolReplayPolicy::Never
}

/// 单次运行（Run）与轮次（Turn）的执行状态记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RunExecutionState {
    #[serde(rename_all = "camelCase")]
    Running {
        run_id: u64,
        process_id: u32,
        started_at_ms: u64,
        last_activity_at_ms: u64,
        in_flight_tools: Vec<InFlightToolRecord>,
    },
    #[serde(rename_all = "camelCase")]
    Completed { ended_at_ms: u64 },
    #[serde(rename_all = "camelCase")]
    Aborted { ended_at_ms: u64 },
    #[serde(rename_all = "camelCase")]
    Error { ended_at_ms: u64, message: String },
    /// 进程强杀、断电或拔盘导致的异常中断。
    #[serde(rename_all = "camelCase")]
    InterruptedByCrash {
        detected_at_ms: u64,
        last_activity_at_ms: u64,
        interrupted_tools: Vec<InFlightToolRecord>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedExecutionLog {
    #[serde(default)]
    pub inputs: BTreeMap<String, InputCorrelationRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_run: Option<RunExecutionState>,
    #[serde(default)]
    pub history_runs: Vec<RunExecutionState>,
}

/// 仅供测试注入状态日志写入/清理失败。
#[cfg(feature = "test-support")]
#[derive(Debug, Clone, Default)]
pub struct ExecutionStateFaultInjection {
    /// 在登记工具执行前状态时模拟持久化失败。
    pub fail_next_tool_start_write: bool,
    /// 在工具结果已写入会话 JSONL 后、清理状态日志时模拟持久化失败。
    pub fail_next_tool_clear_write: bool,
    /// 在通用状态保存时模拟失败。
    pub fail_next_save: bool,
}

pub trait ExecutionStateStore: Send + Sync {
    fn load(&self) -> Result<PersistedExecutionLog, SessionStoreError>;
    fn save(&mut self, state: &PersistedExecutionLog) -> Result<(), SessionStoreError>;
}

// =============================================================================
// 1. InMemoryExecutionStateStore & JsonFileExecutionStateStore
// =============================================================================

#[derive(Debug, Clone, Default)]
pub struct InMemoryExecutionStateStore {
    state: Arc<Mutex<PersistedExecutionLog>>,
    #[cfg(feature = "test-support")]
    fault_injection: Arc<Mutex<ExecutionStateFaultInjection>>,
}

impl InMemoryExecutionStateStore {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(feature = "test-support")]
    pub fn fault_injection(&self) -> Arc<Mutex<ExecutionStateFaultInjection>> {
        self.fault_injection.clone()
    }

    pub fn snapshot(&self) -> PersistedExecutionLog {
        self.state.lock().expect("state lock").clone()
    }
}

impl ExecutionStateStore for InMemoryExecutionStateStore {
    fn load(&self) -> Result<PersistedExecutionLog, SessionStoreError> {
        Ok(self.state.lock().expect("state lock").clone())
    }

    fn save(&mut self, state: &PersistedExecutionLog) -> Result<(), SessionStoreError> {
        #[cfg(feature = "test-support")]
        {
            let mut fault = self.fault_injection.lock().expect("fault lock");
            if fault.fail_next_save {
                fault.fail_next_save = false;
                return Err(SessionStoreError::Io(
                    "Fault-injected ExecutionStateStore save failure".to_owned(),
                ));
            }
        }
        *self.state.lock().expect("state lock") = state.clone();
        Ok(())
    }
}

#[derive(Clone)]
pub struct JsonFileExecutionStateStore {
    path: PathBuf,
    #[cfg(feature = "test-support")]
    fault_injection: Arc<Mutex<ExecutionStateFaultInjection>>,
}

impl std::fmt::Debug for JsonFileExecutionStateStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonFileExecutionStateStore")
            .field("path", &self.path)
            .finish()
    }
}

impl JsonFileExecutionStateStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            #[cfg(feature = "test-support")]
            fault_injection: Arc::new(Mutex::new(ExecutionStateFaultInjection::default())),
        }
    }

    pub fn for_session_file(session_file: &Path) -> Self {
        Self::new(PathBuf::from(format!(
            "{}.state.json",
            session_file.display()
        )))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(feature = "test-support")]
    pub fn fault_injection(&self) -> Arc<Mutex<ExecutionStateFaultInjection>> {
        self.fault_injection.clone()
    }
}

impl ExecutionStateStore for JsonFileExecutionStateStore {
    fn load(&self) -> Result<PersistedExecutionLog, SessionStoreError> {
        if !self.path.exists() {
            return Ok(PersistedExecutionLog::default());
        }
        let raw = fs::read_to_string(&self.path).map_err(|e| {
            SessionStoreError::Io(format!(
                "Failed to read execution state {}: {e}",
                self.path.display()
            ))
        })?;
        if raw.trim().is_empty() {
            return Err(SessionStoreError::Serialization(format!(
                "Execution state is empty: {}",
                self.path.display()
            )));
        }
        serde_json::from_str::<PersistedExecutionLog>(&raw)
            .map_err(|e| SessionStoreError::Serialization(e.to_string()))
    }

    fn save(&mut self, state: &PersistedExecutionLog) -> Result<(), SessionStoreError> {
        #[cfg(feature = "test-support")]
        {
            let mut fault = self.fault_injection.lock().expect("fault lock");
            if fault.fail_next_save {
                fault.fail_next_save = false;
                return Err(SessionStoreError::Io(
                    "Fault-injected JsonFileExecutionStateStore save failure".to_owned(),
                ));
            }
        }

        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            fs::create_dir_all(parent).map_err(|e| {
                SessionStoreError::Io(format!(
                    "Failed to create state parent dir {}: {e}",
                    parent.display()
                ))
            })?;
        }

        let payload = serde_json::to_vec_pretty(state)
            .map_err(|e| SessionStoreError::Serialization(e.to_string()))?;

        let file_name = self
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("state.json");
        let tmp_path = self
            .path
            .with_file_name(format!("{file_name}.tmp.{}", Uuid::new_v4()));

        let write_res = (|| -> Result<(), std::io::Error> {
            let mut tmp_file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp_path)?;
            tmp_file.write_all(&payload)?;
            tmp_file.sync_all()?;
            Ok(())
        })();

        if let Err(err) = write_res {
            let _ = fs::remove_file(&tmp_path);
            return Err(SessionStoreError::Io(format!(
                "Failed to write temporary execution state {}: {err}",
                tmp_path.display()
            )));
        }

        if let Err(err) = atomic_replace_file_sync(&tmp_path, &self.path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(SessionStoreError::Io(format!(
                "Failed to atomically replace execution state {}: {err}",
                self.path.display()
            )));
        }

        Ok(())
    }
}

// =============================================================================
// 2. ExecutionLogController & Startup Reconciliation
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnresolvedToolCallReport {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    pub tool_name: String,
    pub replay_policy: ToolReplayPolicy,
    /// 是否必须先核对系统真实状态（`ToolReplayPolicy::Never`、策略未知或嵌套调用记录不完整时均为 `true`）。
    pub requires_state_verification: bool,
    /// 结果未知的工具调用一律禁止自动重放。
    pub can_auto_replay: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupReconciliationReport {
    pub recovered_accepted_inputs: Vec<String>,
    pub uncertain_inputs: Vec<InputCorrelationRecord>,
    pub marked_crash_run: bool,
    pub idempotently_cleaned_tool_ids: Vec<String>,
    pub unresolved_tool_calls: Vec<UnresolvedToolCallReport>,
}

/// 管理单会话输入回执关联、运行与工具未决记录以及启动崩溃核对的控制器。
pub struct ExecutionLogController {
    store: Box<dyn ExecutionStateStore>,
    state: PersistedExecutionLog,
    #[cfg(feature = "test-support")]
    fault_injection: Arc<Mutex<ExecutionStateFaultInjection>>,
}

impl ExecutionLogController {
    #[cfg(feature = "test-support")]
    pub fn with_fault_injection(mut self, fault: Arc<Mutex<ExecutionStateFaultInjection>>) -> Self {
        self.fault_injection = fault;
        self
    }

    pub fn new(store: Box<dyn ExecutionStateStore>) -> Result<Self, SessionStoreError> {
        let state = store.load()?;
        Ok(Self {
            store,
            state,
            #[cfg(feature = "test-support")]
            fault_injection: Arc::new(Mutex::new(ExecutionStateFaultInjection::default())),
        })
    }

    pub fn in_memory() -> Self {
        let mem = InMemoryExecutionStateStore::new();
        #[cfg(feature = "test-support")]
        let fault = mem.fault_injection();
        let log = Self::new(Box::new(mem)).expect("in-memory execution log");
        #[cfg(feature = "test-support")]
        let log = log.with_fault_injection(fault);
        log
    }

    #[cfg(feature = "test-support")]
    pub fn fault_injection(&self) -> Arc<Mutex<ExecutionStateFaultInjection>> {
        self.fault_injection.clone()
    }

    /// 主动释放底层持久化状态日志存储句柄，替换为已关闭的只读实例。
    pub fn release_persistent_store(&mut self) {
        let snapshot = self.state.clone();
        self.store = Box::new(ClosedExecutionStateStore { state: snapshot });
    }

    pub fn state(&self) -> &PersistedExecutionLog {
        &self.state
    }

    pub fn get_input(&self, client_message_id: &str) -> Option<&InputCorrelationRecord> {
        self.state.inputs.get(client_message_id)
    }

    /// 登记或查询幂等 `client_message_id`：
    /// 若已存在记录则直接返回现有记录（`is_duplicate = true`）；否则原子持久化为 `Delivering`。
    pub fn begin_input_delivery(
        &mut self,
        client_message_id: &str,
        queue_item_id: Option<&str>,
        text: &str,
        images: Vec<ImageContent>,
    ) -> Result<(InputCorrelationRecord, bool), SessionStoreError> {
        if let Some(existing) = self.state.inputs.get(client_message_id) {
            return Ok((existing.clone(), true));
        }

        let now = now_ms();
        let record = InputCorrelationRecord {
            client_message_id: client_message_id.to_owned(),
            queue_item_id: queue_item_id
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| Uuid::new_v4().to_string()),
            text: text.to_owned(),
            images,
            status: InputDeliveryStatus::Delivering,
            uncertain: false,
            created_at_ms: now,
            updated_at_ms: now,
        };

        let mut candidate = self.state.clone();
        candidate
            .inputs
            .insert(client_message_id.to_owned(), record.clone());
        self.store.save(&candidate)?;
        self.state = candidate;
        Ok((record, false))
    }

    /// 当用户消息已正式落盘到 `SessionManager`（或被扩展 `input` 钩子 `Handled` 消化）后，
    /// 写入关联条目并将输入状态原子更新为 `Accepted`。
    pub fn mark_input_accepted(
        &mut self,
        session_manager: &mut SessionManager,
        client_message_id: &str,
        disposition: PromptDisposition,
        user_entry_id: Option<String>,
    ) -> Result<InputCorrelationRecord, SessionStoreError> {
        let Some(existing) = self.state.inputs.get(client_message_id).cloned() else {
            return Err(SessionStoreError::InvalidOperation(format!(
                "Input record {client_message_id} not found"
            )));
        };

        let already_correlated = session_manager.get_entries().iter().any(|entry| {
            matches!(entry, SessionEntry::Custom(c)
                if c.custom_type == INPUT_CORRELATION_CUSTOM_TYPE
                    && c.data.as_ref().and_then(|d| d.get("clientMessageId")).and_then(Value::as_str) == Some(client_message_id))
        });
        if let Some(uid) = &user_entry_id
            && !already_correlated
        {
            session_manager.append_custom_entry(
                INPUT_CORRELATION_CUSTOM_TYPE,
                Some(json!({
                    "clientMessageId": client_message_id,
                    "userEntryId": uid,
                    "queueItemId": existing.queue_item_id,
                })),
            )?;
        }

        let mut updated = existing;
        updated.status = InputDeliveryStatus::Accepted {
            disposition,
            user_entry_id,
        };
        updated.uncertain = false;
        updated.updated_at_ms = now_ms();

        let mut candidate = self.state.clone();
        candidate
            .inputs
            .insert(client_message_id.to_owned(), updated.clone());
        self.store.save(&candidate)?;
        self.state = candidate;
        Ok(updated)
    }

    pub fn mark_input_failed(
        &mut self,
        client_message_id: &str,
        error: impl Into<String>,
    ) -> Result<(), SessionStoreError> {
        let Some(mut updated) = self.state.inputs.get(client_message_id).cloned() else {
            return Ok(());
        };
        updated.status = InputDeliveryStatus::Failed {
            error: error.into(),
        };
        updated.uncertain = false;
        updated.updated_at_ms = now_ms();

        let mut candidate = self.state.clone();
        candidate
            .inputs
            .insert(client_message_id.to_owned(), updated);
        self.store.save(&candidate)?;
        self.state = candidate;
        Ok(())
    }

    pub fn start_run(&mut self, run_id: u64) -> Result<(), SessionStoreError> {
        let now = now_ms();
        let mut candidate = self.state.clone();
        if let Some(prev) = candidate.active_run.take() {
            candidate.history_runs.push(prev);
        }
        candidate.active_run = Some(RunExecutionState::Running {
            run_id,
            process_id: std::process::id(),
            started_at_ms: now,
            last_activity_at_ms: now,
            in_flight_tools: Vec::new(),
        });
        self.store.save(&candidate)?;
        self.state = candidate;
        Ok(())
    }

    /// 在工具进入 `execute` 前可失败地持久化 `InFlightToolRecord` 并刷新 `last_activity_at_ms`。
    pub fn record_tool_start(
        &mut self,
        record: InFlightToolRecord,
    ) -> Result<(), SessionStoreError> {
        #[cfg(feature = "test-support")]
        {
            let mut fault = self.fault_injection.lock().expect("fault lock");
            if fault.fail_next_tool_start_write {
                fault.fail_next_tool_start_write = false;
                return Err(SessionStoreError::Io(
                    "Fault-injected pre-execution tool state write failure".to_owned(),
                ));
            }
        }

        let now = now_ms();
        let mut candidate = self.state.clone();
        match &mut candidate.active_run {
            Some(RunExecutionState::Running {
                last_activity_at_ms,
                in_flight_tools,
                ..
            }) => {
                *last_activity_at_ms = now;
                in_flight_tools.retain(|t| t.tool_call_id != record.tool_call_id);
                in_flight_tools.push(record);
            }
            _ => {
                candidate.active_run = Some(RunExecutionState::Running {
                    run_id: 1,
                    process_id: std::process::id(),
                    started_at_ms: now,
                    last_activity_at_ms: now,
                    in_flight_tools: vec![record],
                });
            }
        }
        self.store.save(&candidate)?;
        self.state = candidate;
        Ok(())
    }

    /// 仅当最终 `ToolResultMessage` 已写入并同步到会话存储后调用：
    /// 移除父 `tool_call_id`，并仅移除 `tr.nested_calls` 中已具有完整终态记录的子调用（缺失或被截断的子调用仍保留未决项）。
    pub fn clear_persisted_tool_result(
        &mut self,
        persisted_result: &ToolResultMessage,
    ) -> Result<(), SessionStoreError> {
        #[cfg(feature = "test-support")]
        {
            let mut fault = self.fault_injection.lock().expect("fault lock");
            if fault.fail_next_tool_clear_write {
                fault.fail_next_tool_clear_write = false;
                return Err(SessionStoreError::Io(
                    "Fault-injected post-persistence tool state clear failure".to_owned(),
                ));
            }
        }

        let parent_id = &persisted_result.tool_call_id;
        let completed_nested_ids: BTreeSet<String> = persisted_result
            .nested_calls
            .as_ref()
            .map(|nc| {
                nc.calls
                    .iter()
                    .filter(|c| nc.complete && matches!(c.status.as_str(), "ok" | "error"))
                    .map(|c| c.id.clone())
                    .collect()
            })
            .unwrap_or_default();

        let now = now_ms();
        let mut candidate = self.state.clone();
        if let Some(RunExecutionState::Running {
            last_activity_at_ms,
            in_flight_tools,
            ..
        }) = &mut candidate.active_run
        {
            *last_activity_at_ms = now;
            let prefix = format!("{parent_id}/");
            in_flight_tools.retain(|rec| {
                if &rec.tool_call_id == parent_id {
                    return false;
                }
                let belongs_to_parent = rec.parent_tool_call_id.as_deref()
                    == Some(parent_id.as_str())
                    || rec.tool_call_id.starts_with(&prefix);
                if belongs_to_parent {
                    // 仅清理在 persisted_result.nested_calls 中已有完整终态记录的子调用；
                    // 缺失或被截断的子调用继续保留在 in_flight_tools 中！
                    !completed_nested_ids.contains(&rec.tool_call_id)
                } else {
                    true
                }
            });
        }

        self.store.save(&candidate)?;
        self.state = candidate;
        Ok(())
    }

    pub fn finish_run(
        &mut self,
        aborted: bool,
        error_message: Option<String>,
    ) -> Result<(), SessionStoreError> {
        let now = now_ms();
        let mut candidate = self.state.clone();
        let remaining_tools = match &candidate.active_run {
            Some(RunExecutionState::Running {
                in_flight_tools, ..
            }) => in_flight_tools.clone(),
            _ => Vec::new(),
        };

        // 若仍有未清理的工具未决项（例如嵌套调用被截断或結果未落盤），保留在 InterruptedByCrash 或历史中供核对
        let finished_state = if !remaining_tools.is_empty() {
            RunExecutionState::InterruptedByCrash {
                detected_at_ms: now,
                last_activity_at_ms: now,
                interrupted_tools: remaining_tools,
            }
        } else if let Some(err) = error_message {
            RunExecutionState::Error {
                ended_at_ms: now,
                message: err,
            }
        } else if aborted {
            RunExecutionState::Aborted { ended_at_ms: now }
        } else {
            RunExecutionState::Completed { ended_at_ms: now }
        };

        candidate.history_runs.push(finished_state);
        candidate.active_run = None;
        self.store.save(&candidate)?;
        self.state = candidate;
        Ok(())
    }
}

fn extract_correlated_inputs(entries: &[SessionEntry]) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for entry in entries {
        if let SessionEntry::Custom(ce) = entry
            && matches!(
                ce.custom_type.as_str(),
                INPUT_CORRELATION_CUSTOM_TYPE | CST_WEB_INPUT_CUSTOM_TYPE
            )
            && let Some(data) = &ce.data
            && let Some(client_id) = data.get("clientMessageId").and_then(Value::as_str)
        {
            let user_entry_id = data
                .get("userEntryId")
                .or_else(|| data.get("entryId"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            map.insert(client_id.to_owned(), user_entry_id);
        }
    }
    map
}

fn collect_persisted_tool_results(entries: &[SessionEntry]) -> BTreeMap<String, ToolResultMessage> {
    let mut map = BTreeMap::new();
    for entry in entries {
        if let SessionEntry::Message(m) = entry
            && let AgentMessage::ToolResult(tr) = &m.message
        {
            map.insert(tr.tool_call_id.clone(), tr.clone());
        }
    }
    map
}

/// 在取得会话单写权并完成 JSONL 尾部核对后执行三步启动恢复核对（对照 `reconcile_on_startup`）。
pub fn reconcile_on_startup(
    session_manager: &mut SessionManager,
    execution_log: &mut ExecutionLogController,
    known_tool_policies: &BTreeMap<String, ToolReplayPolicy>,
) -> Result<StartupReconciliationReport, SessionStoreError> {
    if session_manager.is_poisoned() {
        return Err(SessionStoreError::Poisoned(
            "Cannot reconcile startup state on a poisoned SessionManager".to_owned(),
        ));
    }

    let all_entries = session_manager.get_entries();
    let correlated_inputs = extract_correlated_inputs(&all_entries);
    let persisted_tool_results = collect_persisted_tool_results(&all_entries);

    let mut candidate = execution_log.state.clone();
    let mut report = StartupReconciliationReport::default();
    let now = now_ms();

    // 1. 核对未决输入（Delivering）
    for (client_id, record) in &mut candidate.inputs {
        if matches!(record.status, InputDeliveryStatus::Delivering) {
            if let Some(user_entry_id) = correlated_inputs.get(client_id) {
                record.status = InputDeliveryStatus::Accepted {
                    disposition: PromptDisposition::Started,
                    user_entry_id: (!user_entry_id.is_empty()).then(|| user_entry_id.clone()),
                };
                record.uncertain = false;
                record.updated_at_ms = now;
                report.recovered_accepted_inputs.push(client_id.clone());
            } else {
                record.status = InputDeliveryStatus::Uncertain {
                    reason: "上次进程在投递阶段异常中断，投递结果待核对".to_owned(),
                };
                record.uncertain = true;
                record.updated_at_ms = now;
                report.uncertain_inputs.push(record.clone());
            }
        } else if record.uncertain {
            report.uncertain_inputs.push(record.clone());
        }
    }

    // 2. 核对未闭合运行（Running）与工具未决记录幂等清理
    let mut unresolved_from_log: Vec<InFlightToolRecord> = Vec::new();

    if let Some(active) = candidate.active_run.take() {
        match active {
            RunExecutionState::Running {
                run_id,
                process_id,
                started_at_ms,
                last_activity_at_ms,
                in_flight_tools,
            } => {
                let mut still_interrupted = Vec::new();
                for tool_rec in in_flight_tools {
                    if is_tool_record_persisted_and_complete(&tool_rec, &persisted_tool_results) {
                        report
                            .idempotently_cleaned_tool_ids
                            .push(tool_rec.tool_call_id.clone());
                    } else {
                        still_interrupted.push(tool_rec.clone());
                        unresolved_from_log.push(tool_rec);
                    }
                }

                let crash_state = RunExecutionState::InterruptedByCrash {
                    detected_at_ms: now,
                    last_activity_at_ms,
                    interrupted_tools: still_interrupted.clone(),
                };
                candidate.history_runs.push(crash_state);
                report.marked_crash_run = true;

                session_manager.append_custom_entry(
                    RUN_INTERRUPTED_CUSTOM_TYPE,
                    Some(json!({
                        "runId": run_id,
                        "processId": process_id,
                        "startedAtMs": started_at_ms,
                        "lastActivityAtMs": last_activity_at_ms,
                        "detectedAtMs": now,
                        "endReason": "crash",
                        "interruptedTools": still_interrupted,
                    })),
                )?;
            }
            RunExecutionState::InterruptedByCrash {
                ref interrupted_tools,
                ..
            } => {
                for tool_rec in interrupted_tools {
                    if !is_tool_record_persisted_and_complete(tool_rec, &persisted_tool_results) {
                        unresolved_from_log.push(tool_rec.clone());
                    }
                }
                candidate.history_runs.push(active);
            }
            other => {
                candidate.history_runs.push(other);
            }
        }
    }

    // 同时检查历史记录中残留的 InterruptedByCrash 未决工具并执行幂等清理
    for hist in &mut candidate.history_runs {
        if let RunExecutionState::InterruptedByCrash {
            interrupted_tools, ..
        } = hist
        {
            let mut remaining = Vec::new();
            for tool_rec in interrupted_tools.drain(..) {
                if is_tool_record_persisted_and_complete(&tool_rec, &persisted_tool_results) {
                    if !report
                        .idempotently_cleaned_tool_ids
                        .contains(&tool_rec.tool_call_id)
                    {
                        report
                            .idempotently_cleaned_tool_ids
                            .push(tool_rec.tool_call_id);
                    }
                } else {
                    if !unresolved_from_log
                        .iter()
                        .any(|r| r.tool_call_id == tool_rec.tool_call_id)
                    {
                        unresolved_from_log.push(tool_rec.clone());
                    }
                    remaining.push(tool_rec);
                }
            }
            *interrupted_tools = remaining;
        }
    }

    execution_log.store.save(&candidate)?;
    execution_log.state = candidate;

    // 3. 结合状态日志中的未决工具、分支尾部未闭合 toolCall 以及不完整的 nested_calls 生成恢复核对报告
    let mut seen_unresolved_ids: BTreeSet<String> = BTreeSet::new();

    for rec in unresolved_from_log {
        if seen_unresolved_ids.insert(rec.tool_call_id.clone()) {
            let policy = if rec.replay_policy == ToolReplayPolicy::Never {
                ToolReplayPolicy::Never
            } else {
                known_tool_policies
                    .get(&rec.tool_name)
                    .copied()
                    .unwrap_or(ToolReplayPolicy::Never)
            };
            // 任何未决工具调用（尤其是 Never 策略或嵌套未闭合调用）禁止自动重放；
            // Never 策略或嵌套子调用强制要求核对系统真实状态。
            let requires_verification =
                policy == ToolReplayPolicy::Never || rec.parent_tool_call_id.is_some();
            report.unresolved_tool_calls.push(UnresolvedToolCallReport {
                tool_call_id: rec.tool_call_id,
                parent_tool_call_id: rec.parent_tool_call_id,
                tool_name: rec.tool_name,
                replay_policy: policy,
                requires_state_verification: requires_verification,
                can_auto_replay: false,
                reason: "工具执行在完成最终结果落盘前异常中断，执行结果未知".to_owned(),
            });
        }
    }

    let branch = session_manager.get_branch(None);
    for entry in &branch {
        if let SessionEntry::Message(m) = entry {
            match &m.message {
                AgentMessage::Assistant(a) => {
                    for block in &a.content {
                        if let ContentBlock::ToolCall(tc) = block
                            && !persisted_tool_results.contains_key(&tc.id)
                            && seen_unresolved_ids.insert(tc.id.clone())
                        {
                            let policy = known_tool_policies
                                .get(&tc.name)
                                .copied()
                                .unwrap_or(ToolReplayPolicy::Never);
                            report.unresolved_tool_calls.push(UnresolvedToolCallReport {
                                tool_call_id: tc.id.clone(),
                                parent_tool_call_id: None,
                                tool_name: tc.name.clone(),
                                replay_policy: policy,
                                requires_state_verification: policy == ToolReplayPolicy::Never,
                                can_auto_replay: false,
                                reason: "会话分支尾部存在未写入配对 ToolResult 的工具调用"
                                    .to_owned(),
                            });
                        }
                    }
                }
                AgentMessage::ToolResult(tr) => {
                    if let Some(nested) = &tr.nested_calls {
                        for call in &nested.calls {
                            if call.status == "unfinished"
                                && seen_unresolved_ids.insert(call.id.clone())
                            {
                                let policy = known_tool_policies
                                    .get(&call.name)
                                    .copied()
                                    .unwrap_or(ToolReplayPolicy::Never);
                                report.unresolved_tool_calls.push(UnresolvedToolCallReport {
                                    tool_call_id: call.id.clone(),
                                    parent_tool_call_id: Some(tr.tool_call_id.clone()),
                                    tool_name: call.name.clone(),
                                    replay_policy: policy,
                                    requires_state_verification: true,
                                    can_auto_replay: false,
                                    reason: "父工具结果中的嵌套工具调用未完成".to_owned(),
                                });
                            }
                        }
                        if !nested.complete {
                            let trunc_id = format!("{}/truncated", tr.tool_call_id);
                            if seen_unresolved_ids.insert(trunc_id.clone()) {
                                report.unresolved_tool_calls.push(UnresolvedToolCallReport {
                                    tool_call_id: trunc_id,
                                    parent_tool_call_id: Some(tr.tool_call_id.clone()),
                                    tool_name: tr.tool_name.clone(),
                                    replay_policy: ToolReplayPolicy::Never,
                                    requires_state_verification: true,
                                    can_auto_replay: false,
                                    reason: "父工具结果的 nested_calls 记录因超出上限被截断，子调用终态不完整".to_owned(),
                                });
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    Ok(report)
}

fn is_tool_record_persisted_and_complete(
    rec: &InFlightToolRecord,
    persisted_tool_results: &BTreeMap<String, ToolResultMessage>,
) -> bool {
    if let Some(parent_id) = &rec.parent_tool_call_id {
        // Descendants of descendants are flattened into the top-level result.
        let parent_tr = persisted_tool_results.get(parent_id).or_else(|| {
            persisted_tool_results.values().find(|tr| {
                rec.tool_call_id
                    .starts_with(&format!("{}/", tr.tool_call_id))
                    && tr
                        .nested_calls
                        .as_ref()
                        .is_some_and(|nc| nc.calls.iter().any(|c| c.id == rec.tool_call_id))
            })
        });
        let Some(parent_tr) = parent_tr else {
            return false;
        };
        let Some(nested) = &parent_tr.nested_calls else {
            return false;
        };
        nested.complete
            && nested
                .calls
                .iter()
                .any(|c| c.id == rec.tool_call_id && matches!(c.status.as_str(), "ok" | "error"))
    } else {
        let Some(tr) = persisted_tool_results.get(&rec.tool_call_id) else {
            return false;
        };
        tr.nested_calls.as_ref().is_none_or(|nc| nc.complete)
    }
}

#[derive(Debug, Clone)]
struct ClosedExecutionStateStore {
    state: PersistedExecutionLog,
}

impl ExecutionStateStore for ClosedExecutionStateStore {
    fn load(&self) -> Result<PersistedExecutionLog, SessionStoreError> {
        Ok(self.state.clone())
    }

    fn save(&mut self, _state: &PersistedExecutionLog) -> Result<(), SessionStoreError> {
        Err(SessionStoreError::InvalidOperation(
            "Execution state store has been closed".to_owned(),
        ))
    }
}

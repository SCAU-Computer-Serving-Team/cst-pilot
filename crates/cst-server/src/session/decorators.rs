use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex};

use pi_agent::AgentMessage;
use pi_ai::{ContentBlock, TextContent, ToolResultMessage, UserContentBlock, now_ms};
use pi_session::{AgentSession, AgentSessionEvent, SessionEntry, parse_iso_timestamp_ms};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const INPUT_MESSAGE_CUSTOM_TYPE: &str = "cst-web-input-message";
pub const WORK_DURATION_CUSTOM_TYPE: &str = "cst-web-work-duration";
pub const TOOL_EXECUTION_CUSTOM_TYPE: &str = "cst-web-tool-execution";
pub const BITLOCKER_VERIFICATION_CUSTOM_TYPE: &str = "cst.bitlocker-state-verification";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkDuration {
    pub started_at: u64,
    pub ended_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkRecord {
    pub message_timestamp: u64,
    pub started_at: u64,
    pub ended_at: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionRecord {
    pub message_timestamp: u64,
    pub tool_call_id: String,
    pub started_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<u64>,
    #[serde(skip)]
    pub stage: Option<String>,
    #[serde(skip)]
    pub result: Option<ToolResultMessage>,
    #[serde(skip)]
    pub is_state_verification: bool,
}

struct DecoratorsInner {
    input_ids: HashMap<u64, String>,
    pending_inputs: VecDeque<String>,
    work_records: HashMap<u64, WorkDuration>,
    current_work_started_at: Option<u64>,
    tool_records: HashMap<String, ToolExecutionRecord>,
    active_tools: HashMap<String, String>,
}

type UnsubscribeBox = Box<dyn FnOnce() + Send + Sync>;

/// Subscribes to `AgentSessionEvent` and persists the three display custom entries
/// (`cst-web-input-message`, `cst-web-work-duration`, `cst-web-tool-execution`),
/// decorating outgoing HTTP snapshots and SSE events with `clientMessageId`, `work`, and `execution`.
#[derive(Clone)]
pub struct MessageDecorators {
    session: AgentSession,
    inner: Arc<Mutex<DecoratorsInner>>,
    _unsubscribe: Arc<Mutex<Option<UnsubscribeBox>>>,
}

impl MessageDecorators {
    pub fn attach(session: AgentSession) -> Self {
        let mut inner = DecoratorsInner {
            input_ids: HashMap::new(),
            pending_inputs: VecDeque::new(),
            work_records: HashMap::new(),
            current_work_started_at: None,
            tool_records: HashMap::new(),
            active_tools: HashMap::new(),
        };

        let entries = session.with_session_manager(|sm| sm.get_entries());
        let mut unresolved_bitlocker_calls: Vec<(u64, String)> = Vec::new();
        for entry in entries {
            match entry {
                SessionEntry::Message(msg_entry) => match &msg_entry.message {
                    AgentMessage::Assistant(assistant) => {
                        for part in &assistant.content {
                            if let ContentBlock::ToolCall(tc) = part
                                && tc.name == "bitlocker"
                            {
                                unresolved_bitlocker_calls
                                    .push((assistant.timestamp, tc.id.clone()));
                            }
                        }
                    }
                    AgentMessage::ToolResult(tr) => {
                        unresolved_bitlocker_calls.retain(|(_, id)| id != &tr.tool_call_id);
                        for rec in inner.tool_records.values_mut() {
                            if rec.tool_call_id == tr.tool_call_id && rec.result.is_some() {
                                rec.result = None;
                                rec.is_state_verification = false;
                            }
                        }
                    }
                    _ => {}
                },
                SessionEntry::Custom(custom) => {
                    let Some(data) = custom.data else {
                        continue;
                    };
                    match custom.custom_type.as_str() {
                        INPUT_MESSAGE_CUSTOM_TYPE => {
                            if let (Some(id), Some(ts)) = (
                                data.get("id").and_then(Value::as_str),
                                data.get("timestamp").and_then(Value::as_u64),
                            ) {
                                inner.input_ids.insert(ts, id.to_owned());
                            }
                        }
                        WORK_DURATION_CUSTOM_TYPE => {
                            if let Ok(rec) = serde_json::from_value::<WorkRecord>(data)
                                && rec.ended_at >= rec.started_at
                            {
                                inner.work_records.insert(
                                    rec.message_timestamp,
                                    WorkDuration {
                                        started_at: rec.started_at,
                                        ended_at: rec.ended_at,
                                    },
                                );
                            }
                        }
                        TOOL_EXECUTION_CUSTOM_TYPE => {
                            if let Ok(rec) = serde_json::from_value::<ToolExecutionRecord>(data) {
                                let key = Self::tool_key(rec.message_timestamp, &rec.tool_call_id);
                                inner.tool_records.insert(key, rec);
                            }
                        }
                        BITLOCKER_VERIFICATION_CUSTOM_TYPE => {
                            let verified_at =
                                parse_iso_timestamp_ms(&custom.timestamp).unwrap_or_else(now_ms);
                            let raw_text = serde_json::to_string(&data).unwrap_or_default();
                            for (msg_ts, call_id) in &unresolved_bitlocker_calls {
                                let synthetic = ToolResultMessage {
                                    tool_call_id: call_id.clone(),
                                    tool_name: "bitlocker".to_owned(),
                                    content: vec![UserContentBlock::Text(TextContent {
                                        text: raw_text.clone(),
                                        text_signature: None,
                                    })],
                                    details: Some(data.clone()),
                                    is_error: false,
                                    timestamp: verified_at,
                                    usage: None,
                                    nested_calls: None,
                                    duration_ms: None,
                                };
                                let key = Self::tool_key(*msg_ts, call_id);
                                inner
                                    .tool_records
                                    .entry(key)
                                    .and_modify(|rec| {
                                        rec.ended_at =
                                            Some(rec.ended_at.unwrap_or_else(|| {
                                                rec.started_at.max(verified_at)
                                            }));
                                        rec.result = Some(synthetic.clone());
                                        rec.is_state_verification = true;
                                    })
                                    .or_insert_with(|| ToolExecutionRecord {
                                        message_timestamp: *msg_ts,
                                        tool_call_id: call_id.clone(),
                                        started_at: *msg_ts,
                                        ended_at: Some((*msg_ts).max(verified_at)),
                                        stage: None,
                                        result: Some(synthetic),
                                        is_state_verification: true,
                                    });
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        let shared_inner = Arc::new(Mutex::new(inner));
        let decorator = Self {
            session: session.clone(),
            inner: shared_inner.clone(),
            _unsubscribe: Arc::new(Mutex::new(None)),
        };

        let sub_decorator = decorator.clone();
        let unsub = session.subscribe(Arc::new(move |event| {
            sub_decorator.on_event(&event);
        }));
        *decorator._unsubscribe.lock().expect("unsub lock") = Some(unsub);
        decorator
    }

    fn tool_key(message_timestamp: u64, tool_call_id: &str) -> String {
        format!("{message_timestamp}:{tool_call_id}")
    }

    fn on_event(&self, event: &AgentSessionEvent) {
        match event {
            AgentSessionEvent::MessageStart {
                message: AgentMessage::User(user),
            } => {
                let mut guard = self.inner.lock().expect("decorators lock");
                if let Some(id) = guard.pending_inputs.pop_front() {
                    guard.input_ids.insert(user.timestamp, id);
                }
                guard.current_work_started_at = Some(user.timestamp);
            }
            AgentSessionEvent::MessageEnd { message } => match message {
                AgentMessage::User(user) => {
                    let id = self
                        .inner
                        .lock()
                        .expect("decorators lock")
                        .input_ids
                        .get(&user.timestamp)
                        .cloned();
                    if let Some(id) = id {
                        let _ = self.session.with_session_manager_mut(|sm| {
                            sm.append_custom_entry(
                                INPUT_MESSAGE_CUSTOM_TYPE,
                                Some(json!({"id": id, "timestamp": user.timestamp})),
                            )
                        });
                    }
                }
                AgentMessage::Assistant(assistant) => {
                    let now = now_ms();
                    let record = {
                        let mut guard = self.inner.lock().expect("decorators lock");
                        let started_at =
                            guard.current_work_started_at.unwrap_or(assistant.timestamp);
                        let ended_at = started_at.max(now);
                        guard.work_records.insert(
                            assistant.timestamp,
                            WorkDuration {
                                started_at,
                                ended_at,
                            },
                        );
                        WorkRecord {
                            message_timestamp: assistant.timestamp,
                            started_at,
                            ended_at,
                        }
                    };
                    let _ = self.session.with_session_manager_mut(|sm| {
                        sm.append_custom_entry(
                            WORK_DURATION_CUSTOM_TYPE,
                            serde_json::to_value(record).ok(),
                        )
                    });
                }
                AgentMessage::ToolResult(tr) => {
                    let mut guard = self.inner.lock().expect("decorators lock");
                    for rec in guard.tool_records.values_mut() {
                        if rec.tool_call_id == tr.tool_call_id && rec.result.is_some() {
                            rec.result = None;
                            rec.is_state_verification = false;
                        }
                    }
                }
                _ => {}
            },
            AgentSessionEvent::ToolExecutionStart { tool_call_id, .. } => {
                let assistant_ts = self.session.state().messages.iter().rev().find_map(|msg| {
                    match msg {
                        AgentMessage::Assistant(a)
                            if a.content.iter().any(|part| {
                                matches!(part, ContentBlock::ToolCall(tc) if tc.id == *tool_call_id)
                            }) =>
                        {
                            Some(a.timestamp)
                        }
                        _ => None,
                    }
                });
                let Some(message_timestamp) = assistant_ts else {
                    return;
                };
                let started_at = now_ms();
                let record = ToolExecutionRecord {
                    message_timestamp,
                    tool_call_id: tool_call_id.clone(),
                    started_at,
                    ended_at: None,
                    stage: None,
                    result: None,
                    is_state_verification: false,
                };
                let persisted = serde_json::to_value(&record).ok();
                {
                    let mut guard = self.inner.lock().expect("decorators lock");
                    let key = Self::tool_key(message_timestamp, tool_call_id);
                    guard.tool_records.insert(key.clone(), record);
                    guard.active_tools.insert(tool_call_id.clone(), key);
                }
                let _ = self.session.with_session_manager_mut(|sm| {
                    sm.append_custom_entry(TOOL_EXECUTION_CUSTOM_TYPE, persisted)
                });
            }
            AgentSessionEvent::ToolExecutionUpdate {
                tool_call_id,
                partial_result,
                ..
            } => {
                if let Some(stage) = partial_result
                    .details
                    .get("bitlocker")
                    .and_then(|b| b.get("stage"))
                    .and_then(Value::as_str)
                {
                    let mut guard = self.inner.lock().expect("decorators lock");
                    if let Some(key) = guard.active_tools.get(tool_call_id).cloned()
                        && let Some(rec) = guard.tool_records.get_mut(&key)
                    {
                        rec.stage = Some(stage.to_owned());
                    }
                }
            }
            AgentSessionEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
                duration_ms,
                ..
            } => {
                let now = now_ms();
                let persisted = {
                    let mut guard = self.inner.lock().expect("decorators lock");
                    let Some(key) = guard.active_tools.remove(tool_call_id) else {
                        return;
                    };
                    let Some(rec) = guard.tool_records.get_mut(&key) else {
                        return;
                    };
                    let ended_at = rec.started_at.max(now);
                    rec.ended_at = Some(ended_at);
                    rec.result = Some(ToolResultMessage {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        content: result.content.clone(),
                        details: (!result.details.is_null()).then(|| result.details.clone()),
                        is_error: *is_error,
                        timestamp: ended_at,
                        usage: None,
                        nested_calls: None,
                        duration_ms: *duration_ms,
                    });
                    serde_json::to_value(&*rec).ok()
                };
                let _ = self.session.with_session_manager_mut(|sm| {
                    sm.append_custom_entry(TOOL_EXECUTION_CUSTOM_TYPE, persisted)
                });
            }
            AgentSessionEvent::AgentEnd { .. } => {
                let to_persist: Vec<Value> = {
                    let mut guard = self.inner.lock().expect("decorators lock");
                    guard.pending_inputs.clear();
                    guard.current_work_started_at = None;
                    let active_keys: Vec<String> =
                        guard.active_tools.drain().map(|(_, k)| k).collect();
                    let now = now_ms();
                    let mut out = Vec::new();
                    for key in active_keys {
                        if let Some(rec) = guard.tool_records.get_mut(&key) {
                            rec.ended_at = Some(rec.started_at.max(now));
                            if let Ok(val) = serde_json::to_value(&*rec) {
                                out.push(val);
                            }
                        }
                    }
                    out
                };
                for val in to_persist {
                    let _ = self.session.with_session_manager_mut(|sm| {
                        sm.append_custom_entry(TOOL_EXECUTION_CUSTOM_TYPE, Some(val))
                    });
                }
            }
            _ => {}
        }
    }

    pub async fn track_input<T, F, Fut>(
        &self,
        id: &str,
        deferred: bool,
        action: F,
    ) -> Result<T, String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, String>>,
    {
        self.inner
            .lock()
            .expect("decorators lock")
            .pending_inputs
            .push_back(id.to_owned());
        let res = action().await;
        if res.is_err() || !deferred {
            self.forget_pending_input(id);
        }
        res
    }

    fn forget_pending_input(&self, id: &str) {
        let mut guard = self.inner.lock().expect("decorators lock");
        if let Some(pos) = guard.pending_inputs.iter().position(|item| item == id) {
            guard.pending_inputs.remove(pos);
        }
    }

    pub fn has_input_message(&self, id: &str) -> bool {
        self.inner
            .lock()
            .expect("decorators lock")
            .input_ids
            .values()
            .any(|v| v == id)
    }

    pub fn early_tool_result(
        &self,
        message_timestamp: u64,
        tool_call_id: &str,
    ) -> Option<ToolResultMessage> {
        let key = Self::tool_key(message_timestamp, tool_call_id);
        self.inner
            .lock()
            .expect("decorators lock")
            .tool_records
            .get(&key)
            .and_then(|rec| rec.result.clone())
    }

    pub fn verified_bitlocker_result(
        &self,
        message_timestamp: u64,
        tool_call_id: &str,
    ) -> Option<ToolResultMessage> {
        let key = Self::tool_key(message_timestamp, tool_call_id);
        self.inner
            .lock()
            .expect("decorators lock")
            .tool_records
            .get(&key)
            .filter(|rec| rec.is_state_verification)
            .and_then(|rec| rec.result.clone())
    }

    /// Decorates an `AgentMessage` for HTTP snapshot output (injects `clientMessageId`, `work`, and `execution`).
    pub fn decorate_message_for_snapshot(&self, message: &AgentMessage) -> Value {
        let mut value = serde_json::to_value(message).unwrap_or(Value::Null);
        self.decorate_message_value(&mut value, true);
        value
    }

    /// Decorates an `AgentSessionEvent` for SSE streaming (injects `clientMessageId` on `message`).
    pub fn decorate_event(&self, event: &AgentSessionEvent) -> Value {
        let mut value = serde_json::to_value(event).unwrap_or(Value::Null);
        if let Some(msg) = value.get_mut("message") {
            self.decorate_message_value(msg, false);
        }
        value
    }

    fn decorate_message_value(&self, msg: &mut Value, include_work_and_tools: bool) {
        let Some(role) = msg.get("role").and_then(Value::as_str).map(str::to_owned) else {
            return;
        };
        let Some(ts) = msg.get("timestamp").and_then(Value::as_u64) else {
            return;
        };

        let guard = self.inner.lock().expect("decorators lock");
        if role == "user" {
            if let Some(client_id) = guard.input_ids.get(&ts)
                && let Some(obj) = msg.as_object_mut()
            {
                obj.insert(
                    "clientMessageId".to_owned(),
                    Value::String(client_id.clone()),
                );
            }
            return;
        }

        if include_work_and_tools && role == "assistant" {
            if let Some(work) = guard.work_records.get(&ts)
                && let Some(obj) = msg.as_object_mut()
                && let Ok(work_val) = serde_json::to_value(work)
            {
                obj.insert("work".to_owned(), work_val);
            }
            if let Some(content) = msg.get_mut("content").and_then(Value::as_array_mut) {
                for part in content {
                    if part.get("type").and_then(Value::as_str) != Some("toolCall") {
                        continue;
                    }
                    let Some(tc_id) = part.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    let key = Self::tool_key(ts, tc_id);
                    if let Some(rec) = guard.tool_records.get(&key)
                        && let Some(part_obj) = part.as_object_mut()
                    {
                        let mut exec = serde_json::Map::new();
                        exec.insert("startedAt".to_owned(), json!(rec.started_at));
                        if let Some(ended_at) = rec.ended_at {
                            exec.insert("endedAt".to_owned(), json!(ended_at));
                        }
                        if let Some(stage) = &rec.stage {
                            exec.insert("stage".to_owned(), json!(stage));
                        }
                        if let Some(result) = &rec.result
                            && let Ok(res_val) =
                                serde_json::to_value(AgentMessage::ToolResult(result.clone()))
                        {
                            exec.insert("result".to_owned(), res_val);
                        }
                        part_obj.insert("execution".to_owned(), Value::Object(exec));
                    }
                }
            }
        }
    }

    pub fn close(&self) {
        if let Some(unsub) = self._unsubscribe.lock().expect("unsub lock").take() {
            unsub();
        }
    }
}

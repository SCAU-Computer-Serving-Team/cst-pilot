use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_agent::{AgentMessage, BoxFuture, QueueMode as QueueDeliveryMode};
use pi_ai::ModelThinkingLevel;
use pi_ai::{
    ContentBlock, ImageContent, JsonValue, Model, UserContent, UserContentBlock,
    is_thinking_level_supported,
};
use pi_session::{
    ExecuteBashOptions, ExtensionError, ExtensionMode, ExtensionUiContext, PromptDisposition,
    PromptOptions, SessionEntry, StreamingInputBehavior, UiDialogOptions, UiNotifyLevel,
    WorkingIndicatorOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::modes::json_event::to_json_event;
use crate::modes::print_mode::bind_runtime_command_actions;
use crate::runtime::{
    AgentSessionRuntime, ForkPosition, ForkSessionRuntimeOptions, NewSessionRuntimeOptions,
    SwitchSessionRuntimeOptions,
};

pub const MAX_RPC_FRAME_BYTES: usize = 10 * 1024 * 1024; // 10 MiB
pub const MAX_STDOUT_QUEUE_EVENTS: usize = 256;
pub const MAX_STDOUT_QUEUE_BYTES: usize = 8 * 1024 * 1024; // 8 MiB

const ALL_THINKING_LEVELS: &[ModelThinkingLevel] = &[
    ModelThinkingLevel::Off,
    ModelThinkingLevel::Minimal,
    ModelThinkingLevel::Low,
    ModelThinkingLevel::Medium,
    ModelThinkingLevel::High,
    ModelThinkingLevel::XHigh,
    ModelThinkingLevel::Max,
];

#[derive(Clone)]
pub struct BoundedStdoutQueue {
    tx: mpsc::Sender<(String, Option<tokio::sync::OwnedSemaphorePermit>)>,
    byte_sem: Arc<Semaphore>,
    closed: Arc<AtomicBool>,
    failure: CancellationToken,
}

impl BoundedStdoutQueue {
    pub fn new(
        tx: mpsc::Sender<(String, Option<tokio::sync::OwnedSemaphorePermit>)>,
        byte_sem: Arc<Semaphore>,
        closed: Arc<AtomicBool>,
    ) -> Self {
        Self {
            tx,
            byte_sem,
            closed,
            failure: CancellationToken::new(),
        }
    }

    pub(crate) fn failure_token(&self) -> CancellationToken {
        self.failure.clone()
    }

    pub(crate) fn fail(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.failure.cancel();
    }

    fn encode(&self, val: &Value) -> Result<String, String> {
        if self.closed.load(Ordering::SeqCst) {
            return Err("stdout queue closed".to_owned());
        }
        struct LimitedBuffer(Vec<u8>);
        impl std::io::Write for LimitedBuffer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.0.len().saturating_add(bytes.len()) >= MAX_STDOUT_QUEUE_BYTES {
                    return Err(std::io::Error::other("RPC output byte limit"));
                }
                self.0.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut buffer = LimitedBuffer(Vec::new());
        if serde_json::to_writer(&mut buffer, val).is_err() {
            self.fail();
            return Err(
                "RPC output exceeds maximum byte budget or cannot be serialized".to_owned(),
            );
        }
        buffer.0.push(b'\n');
        String::from_utf8(buffer.0).map_err(|_| "Cannot encode RPC output".to_owned())
    }

    pub async fn send_json(&self, val: &Value) -> Result<(), String> {
        self.send_text(self.encode(val)?).await
    }

    pub(crate) async fn send_text(&self, line: String) -> Result<(), String> {
        if line.len() > MAX_STDOUT_QUEUE_BYTES {
            self.fail();
            return Err("stdout record exceeds byte budget".to_owned());
        }
        let result = tokio::select! {
            biased;
            _ = self.failure.cancelled() => Err("stdout queue closed".to_owned()),
            result = async {
                let permit = self.byte_sem.clone().acquire_many_owned(line.len() as u32).await
                    .map_err(|_| "stdout byte budget closed".to_owned())?;
                self.tx.send((line, Some(permit))).await.map_err(|_| "stdout writer closed".to_owned())
            } => result,
        };
        if result.is_err() {
            self.fail();
        }
        result
    }

    /// Synchronous producers cannot await; exhaustion explicitly fails the connection.
    /// No unbounded buffer, per-notification task or silent protocol loss is permitted.
    pub fn try_send_json(&self, val: &Value) -> Result<(), String> {
        self.try_send_text(self.encode(val)?)
    }

    pub(crate) fn try_send_text(&self, line: String) -> Result<(), String> {
        if line.len() > MAX_STDOUT_QUEUE_BYTES {
            self.fail();
            return Err("stdout record exceeds byte budget".to_owned());
        }
        let result = self
            .byte_sem
            .clone()
            .try_acquire_many_owned(line.len() as u32)
            .map_err(|_| "RPC stdout byte budget exhausted".to_owned())
            .and_then(|permit| {
                self.tx
                    .try_send((line, Some(permit)))
                    .map_err(|_| "RPC stdout event budget exhausted".to_owned())
            });
        if result.is_err() {
            self.fail();
        }
        result
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcUiResponseFrame {
    pub id: String,
    #[serde(default)]
    pub generation: Option<u64>,
    #[serde(default)]
    pub value: Option<Value>,
    #[serde(default)]
    pub confirmed: Option<bool>,
    #[serde(default)]
    pub cancelled: Option<bool>,
}

struct PendingUiRequest {
    generation: u64,
    _secret: bool,
    sender: oneshot::Sender<RpcUiResponseFrame>,
}

struct PendingUiGuard {
    pending: Arc<Mutex<BTreeMap<String, PendingUiRequest>>>,
    id: String,
}

impl Drop for PendingUiGuard {
    fn drop(&mut self) {
        self.pending.lock().expect("pending lock").remove(&self.id);
    }
}

/// 世代安全的 RPC UI 交互桥（实现 `ExtensionUiContext` Trait，对照 `modes/rpc/`）。
#[derive(Clone)]
pub struct RpcUiBridge {
    stdout_queue: BoundedStdoutQueue,
    generation: Arc<AtomicU64>,
    pending: Arc<Mutex<BTreeMap<String, PendingUiRequest>>>,
    editor_text: Arc<Mutex<String>>,
    closed: Arc<AtomicBool>,
}

impl std::fmt::Debug for RpcUiBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RpcUiBridge")
            .field("generation", &self.generation.load(Ordering::SeqCst))
            .field(
                "pending_count",
                &self.pending.lock().expect("pending lock").len(),
            )
            .finish()
    }
}

impl RpcUiBridge {
    pub fn new(stdout_queue: BoundedStdoutQueue) -> Self {
        Self {
            stdout_queue,
            generation: Arc::new(AtomicU64::new(1)),
            pending: Arc::new(Mutex::new(BTreeMap::new())),
            editor_text: Arc::new(Mutex::new(String::new())),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// 当会话发生切换、重载或作废时递增世代号并立即唤醒所有旧挂起弹窗返回默认拒绝值。
    pub fn bump_generation_and_clear(&self) {
        let mut pending = self.pending.lock().expect("pending lock");
        self.generation.fetch_add(1, Ordering::SeqCst);
        pending.clear();
    }

    pub fn close_and_clear(&self) {
        let mut pending = self.pending.lock().expect("pending lock");
        self.closed.store(true, Ordering::SeqCst);
        pending.clear();
    }

    pub fn clear_all_pending(&self) {
        let mut map = self.pending.lock().expect("pending lock");
        map.clear(); // Dropping oneshot::Sender immediately wakes Receiver with Err -> default rejection!
    }

    pub fn handle_response(&self, resp: RpcUiResponseFrame) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        let current_gen = self.current_generation();
        if let Some(resp_gen) = resp.generation
            && resp_gen != current_gen
        {
            return false;
        }
        let entry = self.pending.lock().expect("pending lock").remove(&resp.id);
        let Some(pending) = entry else {
            return false;
        };
        if pending.generation != current_gen {
            return false;
        }
        let _ = pending.sender.send(resp);
        true
    }

    async fn request_dialog(
        &self,
        method: &str,
        mut payload: serde_json::Map<String, Value>,
        opts: Option<UiDialogOptions>,
        cancel: CancellationToken,
    ) -> Option<RpcUiResponseFrame> {
        if self.closed.load(Ordering::SeqCst) || cancel.is_cancelled() {
            return None;
        }
        let id = Uuid::new_v4().to_string();
        let generation = self.current_generation();
        let secret = opts.as_ref().is_some_and(|o| o.secret);
        let timeout_ms = opts.as_ref().and_then(|o| o.timeout_ms);

        payload.insert(
            "type".to_owned(),
            Value::String("extension_ui_request".to_owned()),
        );
        payload.insert("id".to_owned(), Value::String(id.clone()));
        payload.insert("method".to_owned(), Value::String(method.to_owned()));
        payload.insert("generation".to_owned(), json!(generation));
        if let Some(ref dialog_opts) = opts {
            if let Some(ref desc) = dialog_opts.description {
                payload.insert("description".to_owned(), Value::String(desc.clone()));
            }
            if let Some(ref def_val) = dialog_opts.default_value
                && !dialog_opts.secret
            {
                payload.insert("defaultValue".to_owned(), Value::String(def_val.clone()));
            }
            if dialog_opts.secret {
                payload.insert("secret".to_owned(), Value::Bool(true));
            }
            if let Some(tms) = dialog_opts.timeout_ms {
                payload.insert("timeout".to_owned(), json!(tms));
            }
        }

        let (tx, mut rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().expect("pending lock");
            if self.closed.load(Ordering::SeqCst) || self.current_generation() != generation {
                return None;
            }
            pending.insert(
                id.clone(),
                PendingUiRequest {
                    generation,
                    _secret: secret,
                    sender: tx,
                },
            );
        }
        let _pending = PendingUiGuard {
            pending: self.pending.clone(),
            id,
        };
        let request = async {
            let value = Value::Object(payload);
            tokio::select! {
                result = &mut rx => result.ok(),
                sent = self.stdout_queue.send_json(&value) => {
                    sent.ok()?;
                    rx.await.ok()
                }
            }
        };
        let deadline = async {
            if let Some(tms) = timeout_ms {
                tokio::time::sleep(Duration::from_millis(tms)).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        let frame = tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            () = self.stdout_queue.failure.cancelled() => None,
            () = deadline => None,
            result = request => result,
        }?;
        if self.current_generation() != generation || self.closed.load(Ordering::SeqCst) {
            return None;
        }
        if frame.cancelled == Some(true) {
            return None;
        }
        Some(frame)
    }

    fn emit_fire_and_forget(&self, method: &str, mut payload: serde_json::Map<String, Value>) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        payload.insert(
            "type".to_owned(),
            Value::String("extension_ui_request".to_owned()),
        );
        payload.insert("id".to_owned(), Value::String(Uuid::new_v4().to_string()));
        payload.insert("method".to_owned(), Value::String(method.to_owned()));
        let _ = self.stdout_queue.try_send_json(&Value::Object(payload));
    }
}

impl ExtensionUiContext for RpcUiBridge {
    fn has_ui(&self) -> bool {
        !self.closed.load(Ordering::SeqCst) && !self.stdout_queue.failure_token().is_cancelled()
    }

    fn select<'a>(
        &'a self,
        title: &'a str,
        options: &'a [String],
        opts: Option<UiDialogOptions>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Option<String>> {
        let mut payload = serde_json::Map::new();
        payload.insert("title".to_owned(), Value::String(title.to_owned()));
        payload.insert("options".to_owned(), json!(options));
        Box::pin(async move {
            let frame = self.request_dialog("select", payload, opts, cancel).await?;
            frame.value.and_then(|v| v.as_str().map(ToOwned::to_owned))
        })
    }

    fn confirm<'a>(
        &'a self,
        title: &'a str,
        message: &'a str,
        opts: Option<UiDialogOptions>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, bool> {
        let mut payload = serde_json::Map::new();
        payload.insert("title".to_owned(), Value::String(title.to_owned()));
        payload.insert("message".to_owned(), Value::String(message.to_owned()));
        Box::pin(async move {
            let Some(frame) = self.request_dialog("confirm", payload, opts, cancel).await else {
                return false;
            };
            frame
                .confirmed
                .or_else(|| frame.value.as_ref().and_then(Value::as_bool))
                .unwrap_or(false)
        })
    }

    fn input<'a>(
        &'a self,
        title: &'a str,
        placeholder: Option<&'a str>,
        opts: Option<UiDialogOptions>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Option<String>> {
        let mut payload = serde_json::Map::new();
        payload.insert("title".to_owned(), Value::String(title.to_owned()));
        if let Some(ph) = placeholder {
            payload.insert("placeholder".to_owned(), Value::String(ph.to_owned()));
        }
        Box::pin(async move {
            let frame = self.request_dialog("input", payload, opts, cancel).await?;
            frame.value.and_then(|v| v.as_str().map(ToOwned::to_owned))
        })
    }

    fn editor<'a>(
        &'a self,
        title: &'a str,
        prefill: Option<&'a str>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Option<String>> {
        let mut payload = serde_json::Map::new();
        payload.insert("title".to_owned(), Value::String(title.to_owned()));
        if let Some(pf) = prefill {
            payload.insert("prefill".to_owned(), Value::String(pf.to_owned()));
        }
        Box::pin(async move {
            let frame = self.request_dialog("editor", payload, None, cancel).await?;
            frame.value.and_then(|v| v.as_str().map(ToOwned::to_owned))
        })
    }

    fn custom<'a>(
        &'a self,
        custom_payload: JsonValue,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Option<JsonValue>> {
        let mut payload = serde_json::Map::new();
        payload.insert("payload".to_owned(), custom_payload);
        Box::pin(async move {
            let frame = self.request_dialog("custom", payload, None, cancel).await?;
            frame.value
        })
    }

    fn notify(&self, message: &str, level: UiNotifyLevel) {
        let mut payload = serde_json::Map::new();
        payload.insert("message".to_owned(), Value::String(message.to_owned()));
        payload.insert(
            "notifyType".to_owned(),
            serde_json::to_value(level).unwrap_or(Value::String("info".to_owned())),
        );
        self.emit_fire_and_forget("notify", payload);
    }

    fn set_status(&self, key: &str, text: Option<&str>) {
        let mut payload = serde_json::Map::new();
        payload.insert("statusKey".to_owned(), Value::String(key.to_owned()));
        payload.insert(
            "statusText".to_owned(),
            text.map_or(Value::Null, |t| Value::String(t.to_owned())),
        );
        self.emit_fire_and_forget("setStatus", payload);
    }

    fn set_working_message(&self, message: Option<&str>) {
        let mut payload = serde_json::Map::new();
        payload.insert(
            "message".to_owned(),
            message.map_or(Value::Null, |m| Value::String(m.to_owned())),
        );
        self.emit_fire_and_forget("setWorkingMessage", payload);
    }

    fn set_working_visible(&self, visible: bool) {
        let mut payload = serde_json::Map::new();
        payload.insert("visible".to_owned(), Value::Bool(visible));
        self.emit_fire_and_forget("setWorkingVisible", payload);
    }

    fn set_working_indicator(&self, options: Option<WorkingIndicatorOptions>) {
        let mut payload = serde_json::Map::new();
        payload.insert(
            "options".to_owned(),
            options
                .and_then(|o| serde_json::to_value(o).ok())
                .unwrap_or(Value::Null),
        );
        self.emit_fire_and_forget("setWorkingIndicator", payload);
    }

    fn set_hidden_thinking_label(&self, label: Option<&str>) {
        let mut payload = serde_json::Map::new();
        payload.insert(
            "label".to_owned(),
            label.map_or(Value::Null, |l| Value::String(l.to_owned())),
        );
        self.emit_fire_and_forget("setHiddenThinkingLabel", payload);
    }

    fn set_title(&self, title: &str) {
        let mut payload = serde_json::Map::new();
        payload.insert("title".to_owned(), Value::String(title.to_owned()));
        self.emit_fire_and_forget("setTitle", payload);
    }

    fn paste_to_editor(&self, text: &str) {
        self.editor_text.lock().expect("editor lock").push_str(text);
        let mut payload = serde_json::Map::new();
        payload.insert("text".to_owned(), Value::String(text.to_owned()));
        self.emit_fire_and_forget("pasteToEditor", payload);
    }

    fn set_editor_text(&self, text: &str) {
        *self.editor_text.lock().expect("editor lock") = text.to_owned();
        let mut payload = serde_json::Map::new();
        payload.insert("text".to_owned(), Value::String(text.to_owned()));
        self.emit_fire_and_forget("setEditorText", payload);
    }

    fn get_editor_text(&self) -> String {
        self.editor_text.lock().expect("editor lock").clone()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSessionState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<Model>,
    pub thinking_level: ModelThinkingLevel,
    pub is_streaming: bool,
    pub is_compacting: bool,
    pub steering_mode: String,
    pub follow_up_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    pub auto_compaction_enabled: bool,
    pub message_count: usize,
    pub pending_message_count: usize,
}

fn make_rpc_success(id: Option<&str>, command: &str, data: Option<Value>) -> Value {
    let mut obj = serde_json::Map::new();
    if let Some(id_str) = id {
        obj.insert("id".to_owned(), Value::String(id_str.to_owned()));
    }
    obj.insert("type".to_owned(), Value::String("response".to_owned()));
    obj.insert("command".to_owned(), Value::String(command.to_owned()));
    obj.insert("success".to_owned(), Value::Bool(true));
    if let Some(d) = data {
        obj.insert("data".to_owned(), d);
    }
    Value::Object(obj)
}

fn make_rpc_error(id: Option<&str>, command: &str, error: impl Into<String>) -> Value {
    let mut obj = serde_json::Map::new();
    if let Some(id_str) = id {
        obj.insert("id".to_owned(), Value::String(id_str.to_owned()));
    }
    obj.insert("type".to_owned(), Value::String("response".to_owned()));
    obj.insert("command".to_owned(), Value::String(command.to_owned()));
    obj.insert("success".to_owned(), Value::Bool(false));
    obj.insert("error".to_owned(), Value::String(error.into()));
    Value::Object(obj)
}

/// Validated upstream commands plus headless JSONL export and durable input IDs.
#[derive(Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum RpcCommand {
    Prompt {
        message: String,
        images: Option<Vec<ImageContent>>,
        streaming_behavior: Option<RpcStreamingBehavior>,
        client_message_id: Option<String>,
        queue_item_id: Option<String>,
    },
    Steer {
        message: String,
        images: Option<Vec<ImageContent>>,
    },
    FollowUp {
        message: String,
        images: Option<Vec<ImageContent>>,
    },
    Abort,
    ClearQueue,
    GetState,
    CycleModel,
    GetAvailableModels,
    SetModel {
        provider: String,
        model_id: String,
    },
    SetThinkingLevel {
        level: ModelThinkingLevel,
    },
    CycleThinkingLevel,
    GetAvailableThinkingLevels,
    SetSteeringMode {
        mode: RpcQueueMode,
    },
    SetFollowUpMode {
        mode: RpcQueueMode,
    },
    Compact {
        custom_instructions: Option<String>,
    },
    SetAutoCompaction {
        enabled: bool,
    },
    SetAutoRetry {
        enabled: bool,
    },
    AbortRetry,
    AbortBash,
    Bash {
        command: String,
        exclude_from_context: Option<bool>,
    },
    NewSession {
        parent_session: Option<String>,
    },
    SwitchSession {
        session_path: String,
    },
    Fork {
        entry_id: String,
    },
    Clone,
    GetForkMessages,
    GetEntries {
        since: Option<String>,
    },
    GetTree,
    GetLastAssistantText,
    GetMessages,
    GetSessionStats,
    GetCommands,
    SetSessionName {
        name: String,
    },
    ExportJsonl {
        output_path: Option<String>,
    },
    ExportHtml {
        output_path: Option<String>,
    },
}

#[derive(Deserialize)]
pub enum RpcStreamingBehavior {
    #[serde(rename = "steer")]
    Steer,
    #[serde(rename = "followUp", alias = "follow_up")]
    FollowUp,
}

#[derive(Deserialize)]
pub enum RpcQueueMode {
    #[serde(rename = "all")]
    All,
    #[serde(rename = "one-at-a-time")]
    OneAtATime,
}

fn parse_streaming_behavior(val: Option<&Value>) -> Result<Option<StreamingInputBehavior>, String> {
    let Some(v) = val else {
        return Ok(None);
    };
    if v.is_null() {
        return Ok(None);
    }
    match v.as_str() {
        Some("steer") => Ok(Some(StreamingInputBehavior::Steer)),
        Some("followUp" | "follow_up") => Ok(Some(StreamingInputBehavior::FollowUp)),
        _ => Err("Invalid streamingBehavior: expected 'steer' or 'followUp'".to_owned()),
    }
}

fn parse_queue_mode(val: Option<&Value>) -> Result<QueueDeliveryMode, String> {
    match val.and_then(Value::as_str) {
        Some("all") => Ok(QueueDeliveryMode::All),
        Some("one-at-a-time") => Ok(QueueDeliveryMode::OneAtATime),
        _ => Err("Invalid queue mode: expected 'all' or 'one-at-a-time'".to_owned()),
    }
}

fn is_control_or_query_command(cmd_type: &str) -> bool {
    matches!(
        cmd_type,
        "abort"
            | "abort_retry"
            | "abort_bash"
            | "clear_queue"
            | "steer"
            | "follow_up"
            | "get_state"
            | "get_messages"
            | "get_session_stats"
            | "get_commands"
            | "get_available_models"
            | "get_available_thinking_levels"
            | "get_entries"
            | "get_tree"
            | "get_fork_messages"
            | "get_last_assistant_text"
            | "set_steering_mode"
            | "set_follow_up_mode"
            | "set_auto_compaction"
            | "set_auto_retry"
            | "set_session_name"
    )
}

async fn handle_control_or_query_command(
    runtime_host: &AgentSessionRuntime,
    cmd_type: &str,
    id: Option<String>,
    obj: serde_json::Map<String, Value>,
    stdout_queue: &BoundedStdoutQueue,
) {
    if cmd_type == "prompt" {
        handle_worker_command(runtime_host, cmd_type, id, obj, stdout_queue).await;
        return;
    }
    let id_ref = id.as_deref();
    let response = match runtime_host.try_session() {
        Err(err) => make_rpc_error(id_ref, cmd_type, err),
        Ok(session) => match cmd_type {
            "abort" => {
                session.abort().await;
                make_rpc_success(id_ref, cmd_type, None)
            }
            "abort_retry" => {
                session.abort_retry();
                make_rpc_success(id_ref, cmd_type, None)
            }
            "abort_bash" => {
                session.abort_bash();
                make_rpc_success(id_ref, cmd_type, None)
            }
            "clear_queue" => {
                let (steering, follow_up) = session.clear_queue();
                make_rpc_success(
                    id_ref,
                    cmd_type,
                    Some(json!({ "steering": steering, "followUp": follow_up })),
                )
            }
            "steer" => {
                let Some(msg) = obj.get("message").and_then(Value::as_str) else {
                    let _ = stdout_queue
                        .send_json(&make_rpc_error(
                            id_ref,
                            cmd_type,
                            "Missing or invalid 'message' field",
                        ))
                        .await;
                    return;
                };
                let images = obj
                    .get("images")
                    .and_then(|v| serde_json::from_value::<Vec<ImageContent>>(v.clone()).ok());
                match session.steer(msg, images).await {
                    Ok(()) => make_rpc_success(
                        id_ref,
                        cmd_type,
                        Some(json!({ "disposition": "steered" })),
                    ),
                    Err(err) => make_rpc_error(id_ref, cmd_type, err),
                }
            }
            "follow_up" => {
                let Some(msg) = obj.get("message").and_then(Value::as_str) else {
                    let _ = stdout_queue
                        .send_json(&make_rpc_error(
                            id_ref,
                            cmd_type,
                            "Missing or invalid 'message' field",
                        ))
                        .await;
                    return;
                };
                let images = obj
                    .get("images")
                    .and_then(|v| serde_json::from_value::<Vec<ImageContent>>(v.clone()).ok());
                match session.follow_up(msg, images).await {
                    Ok(()) => make_rpc_success(
                        id_ref,
                        cmd_type,
                        Some(json!({ "disposition": "followedUp" })),
                    ),
                    Err(err) => make_rpc_error(id_ref, cmd_type, err),
                }
            }
            "get_state" => {
                let state = session.state();
                let steering_mode = match session.agent().steering_mode() {
                    QueueDeliveryMode::All => "all",
                    QueueDeliveryMode::OneAtATime => "one-at-a-time",
                };
                let follow_up_mode = match session.agent().follow_up_mode() {
                    QueueDeliveryMode::All => "all",
                    QueueDeliveryMode::OneAtATime => "one-at-a-time",
                };
                let rpc_state = RpcSessionState {
                    model: session.configured_model(),
                    thinking_level: session.thinking_level(),
                    is_streaming: session.is_streaming(),
                    is_compacting: session.is_compacting(),
                    steering_mode: steering_mode.to_owned(),
                    follow_up_mode: follow_up_mode.to_owned(),
                    session_file: session
                        .session_file()
                        .map(|p| p.to_string_lossy().into_owned()),
                    session_id: session.session_id(),
                    session_name: session.session_name(),
                    auto_compaction_enabled: session.compaction_settings().enabled,
                    message_count: state.messages.len(),
                    pending_message_count: session.pending_message_count(),
                };
                make_rpc_success(id_ref, cmd_type, serde_json::to_value(rpc_state).ok())
            }
            "get_messages" => {
                let msgs = session.state().messages;
                make_rpc_success(id_ref, cmd_type, Some(json!({ "messages": msgs })))
            }
            "get_session_stats" => {
                let stats = session.get_session_stats();
                make_rpc_success(id_ref, cmd_type, serde_json::to_value(stats).ok())
            }
            "get_commands" => {
                let cmds = session.get_slash_commands();
                make_rpc_success(id_ref, cmd_type, Some(json!({ "commands": cmds })))
            }
            "get_available_models" => {
                let models = runtime_host
                    .try_services()
                    .map(|sv| sv.model_runtime.get_available_models())
                    .unwrap_or_default();
                make_rpc_success(id_ref, cmd_type, Some(json!({ "models": models })))
            }
            "get_available_thinking_levels" => {
                let levels: Vec<ModelThinkingLevel> = match session.configured_model() {
                    Some(m) => ALL_THINKING_LEVELS
                        .iter()
                        .copied()
                        .filter(|&lvl| is_thinking_level_supported(&m, lvl))
                        .collect(),
                    None => vec![ModelThinkingLevel::Off],
                };
                make_rpc_success(id_ref, cmd_type, Some(json!({ "levels": levels })))
            }
            "get_entries" => {
                let since = obj.get("since").and_then(Value::as_str);
                let (entries, leaf_id) = session.with_session_manager(|sm| {
                    let all = sm.get_entries();
                    let filtered = if let Some(since_id) = since {
                        if let Some(pos) = all.iter().position(|e| e.id() == since_id) {
                            all[pos + 1..].to_vec()
                        } else {
                            all
                        }
                    } else {
                        all
                    };
                    (filtered, sm.get_leaf_id().map(ToOwned::to_owned))
                });
                make_rpc_success(
                    id_ref,
                    cmd_type,
                    Some(json!({ "entries": entries, "leafId": leaf_id })),
                )
            }
            "get_tree" => {
                let (tree, leaf_id) = session.with_session_manager(|sm| {
                    (sm.get_tree(), sm.get_leaf_id().map(ToOwned::to_owned))
                });
                make_rpc_success(
                    id_ref,
                    cmd_type,
                    Some(json!({ "tree": tree, "leafId": leaf_id })),
                )
            }
            "get_fork_messages" => {
                let branch = session.with_session_manager(|sm| sm.get_branch(None));
                let mut fork_msgs = Vec::new();
                for entry in branch {
                    if let SessionEntry::Message(m) = entry
                        && let AgentMessage::User(u) = &m.message
                    {
                        let text = match &u.content {
                            UserContent::Text(s) => s.clone(),
                            UserContent::Blocks(blocks) => blocks
                                .iter()
                                .filter_map(|b| match b {
                                    UserContentBlock::Text(t) => Some(t.text.as_str()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join(""),
                        };
                        fork_msgs.push(json!({ "entryId": m.id, "text": text }));
                    }
                }
                make_rpc_success(id_ref, cmd_type, Some(json!({ "messages": fork_msgs })))
            }
            "get_last_assistant_text" => {
                let state = session.state();
                let text = state.messages.iter().rev().find_map(|m| match m {
                    AgentMessage::Assistant(a) => {
                        let joined: String = a
                            .content
                            .iter()
                            .filter_map(|b| match b {
                                ContentBlock::Text(t) => Some(t.text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("");
                        Some(joined)
                    }
                    _ => None,
                });
                make_rpc_success(id_ref, cmd_type, Some(json!({ "text": text })))
            }
            "set_steering_mode" => match parse_queue_mode(obj.get("mode")) {
                Ok(mode) => {
                    session.agent().set_steering_mode(mode);
                    make_rpc_success(id_ref, cmd_type, None)
                }
                Err(err) => make_rpc_error(id_ref, cmd_type, err),
            },
            "set_follow_up_mode" => match parse_queue_mode(obj.get("mode")) {
                Ok(mode) => {
                    session.agent().set_follow_up_mode(mode);
                    make_rpc_success(id_ref, cmd_type, None)
                }
                Err(err) => make_rpc_error(id_ref, cmd_type, err),
            },
            "set_auto_compaction" => {
                let Some(enabled) = obj.get("enabled").and_then(Value::as_bool) else {
                    let _ = stdout_queue
                        .send_json(&make_rpc_error(
                            id_ref,
                            cmd_type,
                            "Missing or invalid 'enabled' field",
                        ))
                        .await;
                    return;
                };
                let mut settings = session.compaction_settings();
                settings.enabled = enabled;
                session.set_compaction_settings(settings);
                make_rpc_success(id_ref, cmd_type, None)
            }
            "set_auto_retry" => {
                let Some(enabled) = obj.get("enabled").and_then(Value::as_bool) else {
                    let _ = stdout_queue
                        .send_json(&make_rpc_error(
                            id_ref,
                            cmd_type,
                            "Missing or invalid 'enabled' field",
                        ))
                        .await;
                    return;
                };
                let mut policy = session.retry_policy();
                policy.enabled = enabled;
                session.set_retry_policy(policy);
                make_rpc_success(id_ref, cmd_type, None)
            }
            "set_session_name" => {
                let Some(name) = obj.get("name").and_then(Value::as_str) else {
                    let _ = stdout_queue
                        .send_json(&make_rpc_error(
                            id_ref,
                            cmd_type,
                            "Missing or invalid 'name' field",
                        ))
                        .await;
                    return;
                };
                match session.set_session_name(name) {
                    Ok(_) => make_rpc_success(id_ref, cmd_type, None),
                    Err(err) => make_rpc_error(id_ref, cmd_type, err.to_string()),
                }
            }
            _ => make_rpc_error(id_ref, cmd_type, format!("Unknown command: {cmd_type}")),
        },
    };
    let _ = stdout_queue.send_json(&response).await;
}

async fn handle_worker_command(
    runtime_host: &AgentSessionRuntime,
    cmd_type: &str,
    id: Option<String>,
    obj: serde_json::Map<String, Value>,
    stdout_queue: &BoundedStdoutQueue,
) {
    let id_ref = id.as_deref();
    match cmd_type {
        "prompt" => {
            let Some(msg) = obj
                .get("message")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
            else {
                let _ = stdout_queue
                    .send_json(&make_rpc_error(
                        id_ref,
                        cmd_type,
                        "Missing or invalid 'message' field",
                    ))
                    .await;
                return;
            };
            let streaming_behavior = match parse_streaming_behavior(obj.get("streamingBehavior")) {
                Ok(b) => b,
                Err(err) => {
                    let _ = stdout_queue
                        .send_json(&make_rpc_error(id_ref, cmd_type, err))
                        .await;
                    return;
                }
            };
            let images = match obj.get("images") {
                None | Some(Value::Null) => None,
                Some(v) => match serde_json::from_value::<Vec<ImageContent>>(v.clone()) {
                    Ok(imgs) => Some(imgs),
                    Err(_) => {
                        let _ = stdout_queue
                            .send_json(&make_rpc_error(id_ref, cmd_type, "Invalid 'images' field"))
                            .await;
                        return;
                    }
                },
            };
            let client_message_id = obj
                .get("clientMessageId")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let queue_item_id = obj
                .get("queueItemId")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);

            let session = match runtime_host.try_session() {
                Ok(s) => s,
                Err(err) => {
                    let _ = stdout_queue
                        .send_json(&make_rpc_error(id_ref, cmd_type, err))
                        .await;
                    return;
                }
            };

            let (admit_tx, admit_rx) = oneshot::channel::<Result<PromptDisposition, String>>();
            let admit_tx_cell = Arc::new(Mutex::new(Some(admit_tx)));
            let admit_cell_cb = admit_tx_cell.clone();

            let prompt_opts = PromptOptions {
                client_message_id,
                queue_item_id,
                expand_prompt_templates: Some(true),
                images,
                streaming_behavior,
                source: Some(pi_session::InputSource::Rpc),
                on_admitted: Some(Arc::new(move |res| {
                    if let Some(tx) = admit_cell_cb.lock().expect("admit lock").take() {
                        let _ = tx.send(res);
                    }
                })),
            };

            // Drive the caller future in this bounded worker. AgentSession owns
            // the run finalizer; admission acknowledgement does not detach another task.
            let prompt = session.prompt(msg, Some(prompt_opts));
            tokio::pin!(prompt);
            let admitted = tokio::select! {
                biased;
                admission = admit_rx => admission.ok(),
                result = &mut prompt => {
                    match result {
                        Ok(disposition) => {
                            let _ = stdout_queue.send_json(&make_rpc_success(id_ref, cmd_type,
                                Some(json!({ "disposition": disposition })))).await;
                        }
                        Err(err) => { let _ = stdout_queue.send_json(&make_rpc_error(id_ref, cmd_type, err)).await; }
                    }
                    return;
                }
            };
            match admitted {
                Some(Ok(disposition)) => {
                    let _ = stdout_queue
                        .send_json(&make_rpc_success(
                            id_ref,
                            cmd_type,
                            Some(json!({ "disposition": disposition })),
                        ))
                        .await;
                }
                Some(Err(err)) => {
                    let _ = stdout_queue
                        .send_json(&make_rpc_error(id_ref, cmd_type, err))
                        .await;
                    return;
                }
                None => {
                    let _ = stdout_queue
                        .send_json(&make_rpc_error(
                            id_ref,
                            cmd_type,
                            "Prompt admission channel closed unexpectedly",
                        ))
                        .await;
                    return;
                }
            }
            if let Err(err) = prompt.await {
                let _ = stdout_queue
                    .send_json(&make_rpc_error(id_ref, cmd_type, err))
                    .await;
            }
        }
        "compact" => {
            let custom_instructions = obj
                .get("customInstructions")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let resp = match runtime_host.try_session() {
                Err(err) => make_rpc_error(id_ref, cmd_type, err),
                Ok(s) => match s.compact(custom_instructions).await {
                    Ok(res) => make_rpc_success(id_ref, cmd_type, serde_json::to_value(res).ok()),
                    Err(err) => make_rpc_error(id_ref, cmd_type, err),
                },
            };
            let _ = stdout_queue.send_json(&resp).await;
        }
        "bash" => {
            let Some(command) = obj
                .get("command")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
            else {
                let _ = stdout_queue
                    .send_json(&make_rpc_error(
                        id_ref,
                        cmd_type,
                        "Missing or invalid 'command' field",
                    ))
                    .await;
                return;
            };
            let exclude_from_context = obj
                .get("excludeFromContext")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let resp = match runtime_host.try_session() {
                Err(err) => make_rpc_error(id_ref, cmd_type, err),
                Ok(s) => match s
                    .execute_bash(
                        command,
                        ExecuteBashOptions {
                            exclude_from_context,
                            cancel_token: None,
                            on_chunk: None,
                            executor: None,
                        },
                    )
                    .await
                {
                    Ok(res) => make_rpc_success(
                        id_ref,
                        cmd_type,
                        Some(json!({
                            "output": res.output,
                            "exitCode": res.exit_code,
                            "cancelled": res.cancelled,
                            "truncated": res.truncated,
                            "fullOutputPath": res.full_output_path,
                        })),
                    ),
                    Err(err) => make_rpc_error(id_ref, cmd_type, err),
                },
            };
            let _ = stdout_queue.send_json(&resp).await;
        }
        "new_session" => {
            let parent_session = obj
                .get("parentSession")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let resp = match runtime_host
                .new_session(Some(NewSessionRuntimeOptions {
                    parent_session,
                    setup: None,
                    with_session: None,
                }))
                .await
            {
                Ok(out) => make_rpc_success(
                    id_ref,
                    cmd_type,
                    Some(json!({ "cancelled": out.cancelled })),
                ),
                Err(err) => make_rpc_error(id_ref, cmd_type, err),
            };
            let _ = stdout_queue.send_json(&resp).await;
        }
        "switch_session" => {
            let Some(session_path) = obj.get("sessionPath").and_then(Value::as_str) else {
                let _ = stdout_queue
                    .send_json(&make_rpc_error(
                        id_ref,
                        cmd_type,
                        "Missing or invalid 'sessionPath' field",
                    ))
                    .await;
                return;
            };
            let resp = match runtime_host
                .switch_session(session_path, Some(SwitchSessionRuntimeOptions::default()))
                .await
            {
                Ok(out) => make_rpc_success(
                    id_ref,
                    cmd_type,
                    Some(json!({ "cancelled": out.cancelled })),
                ),
                Err(err) => make_rpc_error(id_ref, cmd_type, err),
            };
            let _ = stdout_queue.send_json(&resp).await;
        }
        "fork" => {
            let Some(entry_id) = obj.get("entryId").and_then(Value::as_str) else {
                let _ = stdout_queue
                    .send_json(&make_rpc_error(
                        id_ref,
                        cmd_type,
                        "Missing or invalid 'entryId' field",
                    ))
                    .await;
                return;
            };
            let resp = match runtime_host
                .fork(
                    entry_id,
                    Some(ForkSessionRuntimeOptions {
                        position: Some(ForkPosition::Before),
                        with_session: None,
                    }),
                )
                .await
            {
                Ok(out) => make_rpc_success(
                    id_ref,
                    cmd_type,
                    Some(json!({
                        "text": out.selected_text.unwrap_or_default(),
                        "cancelled": out.cancelled,
                    })),
                ),
                Err(err) => make_rpc_error(id_ref, cmd_type, err),
            };
            let _ = stdout_queue.send_json(&resp).await;
        }
        "clone" => {
            let leaf_opt = runtime_host
                .try_session()
                .ok()
                .and_then(|s| s.with_session_manager(|sm| sm.get_leaf_id().map(ToOwned::to_owned)));
            let resp = match leaf_opt {
                Some(leaf_id) => match runtime_host
                    .fork(
                        &leaf_id,
                        Some(ForkSessionRuntimeOptions {
                            position: Some(ForkPosition::At),
                            with_session: None,
                        }),
                    )
                    .await
                {
                    Ok(out) => make_rpc_success(
                        id_ref,
                        cmd_type,
                        Some(json!({ "cancelled": out.cancelled })),
                    ),
                    Err(err) => make_rpc_error(id_ref, cmd_type, err),
                },
                None => match runtime_host.new_session(None).await {
                    Ok(out) => make_rpc_success(
                        id_ref,
                        cmd_type,
                        Some(json!({ "cancelled": out.cancelled })),
                    ),
                    Err(err) => make_rpc_error(id_ref, cmd_type, err),
                },
            };
            let _ = stdout_queue.send_json(&resp).await;
        }
        "set_model" => {
            let (Some(provider), Some(model_id)) = (
                obj.get("provider").and_then(Value::as_str),
                obj.get("modelId").and_then(Value::as_str),
            ) else {
                let _ = stdout_queue
                    .send_json(&make_rpc_error(
                        id_ref,
                        cmd_type,
                        "Missing 'provider' or 'modelId' field",
                    ))
                    .await;
                return;
            };
            let resp = match (runtime_host.try_session(), runtime_host.try_services()) {
                (Ok(session), Ok(services)) => {
                    if let Some(model) = services.model_runtime.get_model(provider, model_id) {
                        match session.set_model(model.clone()).await {
                            Ok(()) => {
                                make_rpc_success(id_ref, cmd_type, serde_json::to_value(model).ok())
                            }
                            Err(err) => make_rpc_error(id_ref, cmd_type, err),
                        }
                    } else {
                        make_rpc_error(
                            id_ref,
                            cmd_type,
                            format!("Model not found: {provider}/{model_id}"),
                        )
                    }
                }
                (Err(e), _) | (_, Err(e)) => make_rpc_error(id_ref, cmd_type, e),
            };
            let _ = stdout_queue.send_json(&resp).await;
        }
        "cycle_model" => {
            let resp = match (runtime_host.try_session(), runtime_host.try_services()) {
                (Ok(session), Ok(services)) => {
                    let available = services.model_runtime.get_available_models();
                    if available.is_empty() {
                        make_rpc_success(id_ref, cmd_type, Some(Value::Null))
                    } else {
                        let current = session.configured_model();
                        let next_idx = current
                            .as_ref()
                            .and_then(|c| {
                                available
                                    .iter()
                                    .position(|m| m.provider == c.provider && m.id == c.id)
                            })
                            .map_or(0, |idx| (idx + 1) % available.len());
                        let next_model = available[next_idx].clone();
                        match session.set_model(next_model.clone()).await {
                            Ok(()) => make_rpc_success(
                                id_ref,
                                cmd_type,
                                Some(json!({
                                    "model": next_model,
                                    "thinkingLevel": session.thinking_level(),
                                    "isScoped": false,
                                })),
                            ),
                            Err(err) => make_rpc_error(id_ref, cmd_type, err),
                        }
                    }
                }
                (Err(e), _) | (_, Err(e)) => make_rpc_error(id_ref, cmd_type, e),
            };
            let _ = stdout_queue.send_json(&resp).await;
        }
        "set_thinking_level" => {
            let Some(lvl_str) = obj.get("level").and_then(Value::as_str) else {
                let _ = stdout_queue
                    .send_json(&make_rpc_error(
                        id_ref,
                        cmd_type,
                        "Missing or invalid 'level' field",
                    ))
                    .await;
                return;
            };
            let Some(lvl) = pi_session::parse_model_thinking_level(lvl_str) else {
                let _ = stdout_queue
                    .send_json(&make_rpc_error(
                        id_ref,
                        cmd_type,
                        format!("Invalid thinking level: {lvl_str}"),
                    ))
                    .await;
                return;
            };
            let resp = match runtime_host.try_session() {
                Ok(s) => match s.set_thinking_level(lvl) {
                    Ok(()) => make_rpc_success(id_ref, cmd_type, None),
                    Err(err) => make_rpc_error(id_ref, cmd_type, err),
                },
                Err(err) => make_rpc_error(id_ref, cmd_type, err),
            };
            let _ = stdout_queue.send_json(&resp).await;
        }
        "cycle_thinking_level" => {
            let resp = match runtime_host.try_session() {
                Ok(session) => match session.configured_model() {
                    None => make_rpc_success(id_ref, cmd_type, Some(Value::Null)),
                    Some(m) => {
                        let levels: Vec<ModelThinkingLevel> = ALL_THINKING_LEVELS
                            .iter()
                            .copied()
                            .filter(|&l| is_thinking_level_supported(&m, l))
                            .collect();
                        if levels.len() <= 1 {
                            make_rpc_success(id_ref, cmd_type, Some(Value::Null))
                        } else {
                            let cur = session.thinking_level();
                            let next_idx = levels
                                .iter()
                                .position(|&l| l == cur)
                                .map_or(0, |i| (i + 1) % levels.len());
                            let next_lvl = levels[next_idx];
                            match session.set_thinking_level(next_lvl) {
                                Ok(()) => make_rpc_success(
                                    id_ref,
                                    cmd_type,
                                    Some(json!({ "level": next_lvl })),
                                ),
                                Err(err) => make_rpc_error(id_ref, cmd_type, err),
                            }
                        }
                    }
                },
                Err(err) => make_rpc_error(id_ref, cmd_type, err),
            };
            let _ = stdout_queue.send_json(&resp).await;
        }
        "export_jsonl" => {
            let output_path = obj
                .get("outputPath")
                .and_then(Value::as_str)
                .map(PathBuf::from);
            let resp = match runtime_host.try_session() {
                Ok(s) => match s.export_to_jsonl(output_path.as_deref()) {
                    Ok(path) => make_rpc_success(
                        id_ref,
                        cmd_type,
                        Some(json!({ "path": path.to_string_lossy() })),
                    ),
                    Err(err) => make_rpc_error(id_ref, cmd_type, err.to_string()),
                },
                Err(err) => make_rpc_error(id_ref, cmd_type, err),
            };
            let _ = stdout_queue.send_json(&resp).await;
        }
        "export_html" => {
            let resp = make_rpc_error(
                id_ref,
                cmd_type,
                "export_html requires the browser rendering service (Phase 8); use export_jsonl in headless runtime",
            );
            let _ = stdout_queue.send_json(&resp).await;
        }
        _ => {
            let _ = stdout_queue
                .send_json(&make_rpc_error(
                    id_ref,
                    cmd_type,
                    format!("Unknown command: {cmd_type}"),
                ))
                .await;
        }
    }
}

/// 逐块按严格 `\n`（剥离可选 `\r`）读取一帧，单帧超过 `max_bytes` 时有界跳过该行剩余字节并返回 `Err(OversizedFrame)`。
/// 绝不在 `U+2028` / `U+2029` 处断行，也绝不先用不限长的 `read_line` 分配超大内存。
enum FrameReadOutcome {
    Eof,
    Frame(Vec<u8>),
    OversizedFrame,
    IoError(String),
}

async fn read_strict_lf_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max_bytes: usize,
) -> FrameReadOutcome {
    use tokio::io::AsyncBufReadExt;
    let mut frame: Vec<u8> = Vec::new();
    let mut discarding_oversized = false;

    loop {
        let buf = match reader.fill_buf().await {
            Ok(b) => b,
            Err(err) => return FrameReadOutcome::IoError(err.to_string()),
        };
        if buf.is_empty() {
            if discarding_oversized {
                return FrameReadOutcome::OversizedFrame;
            }
            if frame.is_empty() {
                return FrameReadOutcome::Eof;
            }
            if frame.last() == Some(&b'\r') {
                frame.pop();
            }
            return FrameReadOutcome::Frame(frame);
        }

        if let Some(nl_pos) = buf.iter().position(|&b| b == b'\n') {
            if !discarding_oversized {
                if frame.len().saturating_add(nl_pos) > max_bytes {
                    frame.clear();
                    discarding_oversized = true;
                } else {
                    frame.extend_from_slice(&buf[..nl_pos]);
                }
            }
            reader.consume(nl_pos + 1);
            if discarding_oversized {
                return FrameReadOutcome::OversizedFrame;
            }
            if frame.last() == Some(&b'\r') {
                frame.pop();
            }
            return FrameReadOutcome::Frame(frame);
        }

        let chunk_len = buf.len();
        if !discarding_oversized {
            if frame.len().saturating_add(chunk_len) > max_bytes {
                frame.clear();
                discarding_oversized = true;
            } else {
                frame.extend_from_slice(buf);
            }
        }
        reader.consume(chunk_len);
    }
}

/// 运行 JSON Lines RPC 模式（对照 `runRpcMode`）：
/// 解耦 `stdin` 读取泵、有界控制通道、有界工作通道与单一有界 `stdout` 写入器。
pub async fn run_rpc_mode<R, W, E>(
    runtime_host: &AgentSessionRuntime,
    mut stdin: R,
    mut stdout: W,
    mut stderr: E,
) -> i32
where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
    E: AsyncWrite + Unpin + Send + 'static,
{
    let (out_tx, mut out_rx) = mpsc::channel::<(String, Option<tokio::sync::OwnedSemaphorePermit>)>(
        MAX_STDOUT_QUEUE_EVENTS,
    );
    let byte_sem = Arc::new(Semaphore::new(MAX_STDOUT_QUEUE_BYTES));
    let out_closed = Arc::new(AtomicBool::new(false));
    let stdout_queue = BoundedStdoutQueue::new(out_tx, byte_sem, out_closed.clone());

    let (err_tx, mut err_rx) = mpsc::channel::<String>(64);
    let ui_bridge = Arc::new(RpcUiBridge::new(stdout_queue.clone()));

    let rt_for_writer = runtime_host.clone();
    let ui_for_writer = ui_bridge.clone();
    let queue_for_writer = stdout_queue.clone();
    let failure_out = stdout_queue.failure.clone();
    let finish = CancellationToken::new();
    let finish_out = finish.clone();
    let finish_err = finish.clone();
    let writer_handle = tokio::spawn(async move {
        let mut failed = false;
        while let Some((line, _permit)) = tokio::select! {
            line = out_rx.recv() => line,
            () = finish_out.cancelled() => { out_rx.close(); out_rx.recv().await }
        } {
            let result = tokio::select! {
                biased;
                () = failure_out.cancelled() => Err(std::io::Error::other("RPC output cancelled")),
                result = async { stdout.write_all(line.as_bytes()).await?; stdout.flush().await } => result,
            };
            if result.is_err() {
                failed = true;
                queue_for_writer.fail();
                ui_for_writer.close_and_clear();
                if let Ok(s) = rt_for_writer.try_session() {
                    s.dispose();
                }
                break;
            }
        }
        failed
    });

    let queue_for_errors = stdout_queue.clone();
    let err_writer_handle = tokio::spawn(async move {
        while let Some(line) = tokio::select! {
            line = err_rx.recv() => line,
            () = finish_err.cancelled() => { err_rx.close(); err_rx.recv().await }
        } {
            let result = tokio::select! {
                biased;
                () = queue_for_errors.failure.cancelled() => break,
                result = async { stderr.write_all(line.as_bytes()).await?; stderr.flush().await } => result,
            };
            if result.is_err() {
                queue_for_errors.fail();
                break;
            }
        }
    });

    let ui_for_invalidate = ui_bridge.clone();
    runtime_host.set_before_session_invalidate(Some(Arc::new(move || {
        ui_for_invalidate.bump_generation_and_clear();
    })));

    let unsub_holder: Arc<Mutex<Option<super::Unsubscribe>>> = Arc::new(Mutex::new(None));

    let rebind_fn = {
        let rt = runtime_host.clone();
        let queue = stdout_queue.clone();
        let etx = err_tx.clone();
        let ui = ui_bridge.clone();
        let holder = unsub_holder.clone();
        Arc::new(move |_new_session: pi_session::AgentSession| {
            let rt = rt.clone();
            let queue = queue.clone();
            let etx = etx.clone();
            let ui = ui.clone();
            let holder = holder.clone();
            Box::pin(async move {
                if let Some(prev_unsub) = holder.lock().expect("unsub lock").take() {
                    prev_unsub();
                }
                bind_runtime_command_actions(&rt);
                if let Ok(s) = rt.try_session() {
                    s.extension_runner()
                        .set_ui_context(Some(ui.clone()), ExtensionMode::Rpc);
                    let etx_err = etx.clone();
                    let queue_err = queue.clone();
                    s.extension_runner()
                        .on_error(Arc::new(move |err: &ExtensionError| {
                            let mut diagnostic: String =
                                format!("Extension error ({}): {}", err.extension_path, err.error)
                                    .chars()
                                    .take(4096)
                                    .collect();
                            diagnostic.push('\n');
                            if etx_err.try_send(diagnostic).is_err() {
                                queue_err.fail();
                            }
                        }));
                    let q_ev = queue.clone();
                    let unsub = s.subscribe_async(Arc::new(move |event| {
                        let q_inner = q_ev.clone();
                        Box::pin(async move {
                            match to_json_event(&event) {
                                Ok(val) => {
                                    let _ = q_inner.send_json(&val).await;
                                }
                                Err(_) => q_inner.fail(),
                            }
                        })
                    }));
                    *holder.lock().expect("unsub lock") = Some(unsub);
                }
                Ok(())
            }) as BoxFuture<'static, Result<(), String>>
        })
    };

    runtime_host.set_rebind_session(Some(rebind_fn.clone()));
    if let Ok(initial_session) = runtime_host.try_session() {
        let _ = rebind_fn(initial_session).await;
    }

    let (ctrl_tx, mut ctrl_rx) =
        mpsc::channel::<(String, Option<String>, serde_json::Map<String, Value>)>(64);
    let (work_tx, mut work_rx) =
        mpsc::channel::<(String, Option<String>, serde_json::Map<String, Value>)>(64);

    let work_cancel = CancellationToken::new();
    let cancel_controls = work_cancel.clone();
    let rt_ctrl = runtime_host.clone();
    let q_ctrl = stdout_queue.clone();
    let ctrl_handle = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        let mut input_open = true;
        loop {
            tokio::select! {
                command = ctrl_rx.recv(), if input_open && tasks.len() < 16 => {
                    if let Some((cmd_type, id, obj)) = command {
                        let (rt, queue) = (rt_ctrl.clone(), q_ctrl.clone());
                        let cancel = cancel_controls.clone();
                        tasks.spawn(async move {
                            if matches!(cmd_type.as_str(), "prompt" | "steer" | "follow_up") {
                                tokio::select! {
                                    biased;
                                    () = cancel.cancelled() => {
                                        let _ = queue.send_json(&make_rpc_error(id.as_deref(), &cmd_type, "RPC input closed; command cancelled")).await;
                                    }
                                    () = handle_control_or_query_command(&rt, &cmd_type, id.clone(), obj, &queue) => {}
                                }
                            } else {
                                handle_control_or_query_command(&rt, &cmd_type, id, obj, &queue).await;
                            }
                        });
                    } else { input_open = false; }
                }
                result = tasks.join_next(), if !tasks.is_empty() => {
                    if result.is_some_and(|r| r.is_err()) { q_ctrl.fail(); }
                }
                else => break,
            }
        }
    });

    let rt_work = runtime_host.clone();
    let q_work = stdout_queue.clone();
    let cancel_worker = work_cancel.clone();
    let work_handle = tokio::spawn(async move {
        while let Some((cmd_type, id, obj)) = work_rx.recv().await {
            if cancel_worker.is_cancelled() {
                let _ = q_work
                    .send_json(&make_rpc_error(
                        id.as_deref(),
                        &cmd_type,
                        "RPC input closed; command cancelled",
                    ))
                    .await;
                continue;
            }
            if matches!(
                cmd_type.as_str(),
                "new_session" | "switch_session" | "fork" | "clone"
            ) {
                // A structural transition owns its teardown/commit. Do not drop it
                // between releasing the old session and installing its replacement.
                handle_worker_command(&rt_work, &cmd_type, id, obj, &q_work).await;
                continue;
            }
            tokio::select! {
                biased;
                () = cancel_worker.cancelled() => {
                    let _ = q_work.send_json(&make_rpc_error(id.as_deref(), &cmd_type, "RPC input closed; command cancelled")).await;
                }
                () = handle_worker_command(&rt_work, &cmd_type, id.clone(), obj, &q_work) => {}
            }
        }
    });

    // Stdin reader pump
    let mut input_failed = false;
    loop {
        if out_closed.load(Ordering::SeqCst) {
            break;
        }
        let frame = tokio::select! {
            biased;
            () = stdout_queue.failure.cancelled() => break,
            frame = read_strict_lf_frame(&mut stdin, MAX_RPC_FRAME_BYTES) => frame,
        };
        match frame {
            FrameReadOutcome::Eof => break,
            FrameReadOutcome::IoError(err) => {
                input_failed = true;
                let _ = err_tx.try_send(format!("RPC stdin read error: {err}\n"));
                break;
            }
            FrameReadOutcome::OversizedFrame => {
                let _ = stdout_queue.try_send_json(&make_rpc_error(
                    None,
                    "frame",
                    format!("RPC frame exceeds maximum size limit ({MAX_RPC_FRAME_BYTES} bytes)"),
                ));
            }
            FrameReadOutcome::Frame(bytes) => {
                let Ok(line_str) = std::str::from_utf8(&bytes) else {
                    let _ = stdout_queue.try_send_json(&make_rpc_error(
                        None,
                        "parse",
                        "RPC frame is not valid UTF-8",
                    ));
                    continue;
                };
                if line_str.trim().is_empty() {
                    continue;
                }
                let parsed: Value = match serde_json::from_str(line_str) {
                    Ok(v) => v,
                    Err(_) => {
                        let _ = stdout_queue.try_send_json(&make_rpc_error(
                            None,
                            "parse",
                            "Invalid JSON in RPC command",
                        ));
                        continue;
                    }
                };
                let Some(obj) = parsed.as_object().cloned() else {
                    let _ = stdout_queue.try_send_json(&make_rpc_error(
                        None,
                        "parse",
                        "RPC command must be a JSON object",
                    ));
                    continue;
                };
                let id = match obj.get("id") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(s)) => Some(s.clone()),
                    Some(_) => {
                        let _ = stdout_queue.try_send_json(&make_rpc_error(
                            None,
                            "parse",
                            "Invalid 'id' field: expected string",
                        ));
                        continue;
                    }
                };
                let Some(cmd_type) = obj
                    .get("type")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                else {
                    let _ = stdout_queue.try_send_json(&make_rpc_error(
                        id.as_deref(),
                        "unknown",
                        "Missing or invalid 'type' field in RPC frame",
                    ));
                    continue;
                };

                if cmd_type == "extension_ui_response" {
                    // UI 响应立即唤醒挂起弹窗，不产生普通 command response，且绝不等待工作通道
                    if let Ok(ui_resp) =
                        serde_json::from_value::<RpcUiResponseFrame>(Value::Object(obj))
                    {
                        ui_bridge.handle_response(ui_resp);
                    }
                    continue;
                }

                if serde_json::from_value::<RpcCommand>(Value::Object(obj.clone())).is_err() {
                    let _ = stdout_queue.try_send_json(&make_rpc_error(
                        id.as_deref(),
                        &cmd_type,
                        "Unknown command or invalid field type",
                    ));
                    continue;
                }
                if cmd_type == "abort_retry" {
                    if let Ok(session) = runtime_host.try_session() {
                        session.abort_retry();
                    }
                } else if matches!(cmd_type.as_str(), "abort" | "abort_bash") {
                    ui_bridge.clear_all_pending();
                    if let Ok(session) = runtime_host.try_session() {
                        if cmd_type == "abort_bash" {
                            session.abort_bash();
                        } else {
                            session.request_abort();
                        }
                    }
                }
                let streaming_prompt = cmd_type == "prompt"
                    && obj.contains_key("streamingBehavior")
                    && runtime_host
                        .try_session()
                        .is_ok_and(|session| session.is_streaming());
                let channel = if is_control_or_query_command(&cmd_type) || streaming_prompt {
                    &ctrl_tx
                } else {
                    &work_tx
                };
                if let Err(err) = channel.try_send((cmd_type, id, obj)) {
                    let (command, id, _) = err.into_inner();
                    let _ = stdout_queue.try_send_json(&make_rpc_error(
                        id.as_deref(),
                        &command,
                        "RPC command queue is full or closed",
                    ));
                }
            }
        }
    }

    // Stdin EOF or error: immediately clear pending UI prompts so any in-flight tool waiting on confirm/input unblocks!
    ui_bridge.close_and_clear();
    work_cancel.cancel();
    runtime_host.prepare_dispose();
    if let Ok(session) = runtime_host.try_session() {
        session.request_abort();
    }
    drop(ctrl_tx);
    drop(work_tx);
    if ctrl_handle.await.is_err() {
        stdout_queue.fail();
    }
    if work_handle.await.is_err() {
        stdout_queue.fail();
    }

    if let Some(prev_unsub) = unsub_holder.lock().expect("unsub lock").take() {
        prev_unsub();
    }
    let dispose_failed = runtime_host.dispose().await.is_err();
    runtime_host.set_rebind_session(None);
    runtime_host.set_before_session_invalidate(None);
    finish.cancel();

    drop(stdout_queue);
    drop(err_tx);
    let write_failed = writer_handle.await.unwrap_or(true);
    let _ = err_writer_handle.await;

    if input_failed || write_failed || dispose_failed || out_closed.load(Ordering::SeqCst) {
        1
    } else {
        0
    }
}

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use cst_pilot::bitlocker::{
    BitLockerAnswer, BitLockerHostChannel, BitLockerPendingKind,
    credentials::validate as validate_recovery_password, model::Volume,
};
use pi_agent::{AgentMessage, BoxFuture};
use pi_ai::{ContentBlock, now_ms};
use pi_session::{
    AgentSession, ExtensionUiContext, SessionEntry, UiDialogOptions, UiNotifyLevel,
    WorkingIndicatorOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zeroize::Zeroizing;

// =============================================================================
// 1. Generic Extension UI Bridge (`ServerUiBridge`) with Hard Secret Isolation
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuestionKind {
    Select,
    Confirm,
    Input,
    Editor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub request_id: String,
    pub kind: QuestionKind,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefill: Option<String>,
}

enum UiAnswer {
    Text(Option<String>),
    Confirm(bool),
}

struct PendingUiItem {
    question: Question,
    sender: oneshot::Sender<UiAnswer>,
}

struct UiBridgeInner {
    closed: bool,
    pending: BTreeMap<String, PendingUiItem>,
    on_change: Option<Arc<dyn Fn(Vec<Question>) + Send + Sync>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiRespondError {
    Expired(&'static str),
    InvalidInput(&'static str),
}

#[derive(Clone)]
pub struct ServerUiBridge {
    inner: Arc<Mutex<UiBridgeInner>>,
}

impl Default for ServerUiBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerUiBridge {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(UiBridgeInner {
                closed: false,
                pending: BTreeMap::new(),
                on_change: None,
            })),
        }
    }

    pub fn set_on_change(&self, listener: impl Fn(Vec<Question>) + Send + Sync + 'static) {
        self.inner.lock().expect("ui bridge lock").on_change = Some(Arc::new(listener));
    }

    pub fn snapshot(&self) -> Vec<Question> {
        self.inner
            .lock()
            .expect("ui bridge lock")
            .pending
            .values()
            .map(|item| item.question.clone())
            .collect()
    }

    pub fn has_pending(&self) -> bool {
        !self
            .inner
            .lock()
            .expect("ui bridge lock")
            .pending
            .is_empty()
    }

    pub fn respond(&self, request_id: &str, value: &Value) -> Result<(), UiRespondError> {
        let (sender, answer, snapshot, listener) = {
            let mut guard = self.inner.lock().expect("ui bridge lock");
            let Some(item) = guard.pending.get(request_id) else {
                return Err(UiRespondError::Expired("提问已失效"));
            };
            let parsed = match item.question.kind {
                QuestionKind::Confirm => {
                    let Some(flag) = value.as_bool() else {
                        return Err(UiRespondError::InvalidInput("回答格式无效"));
                    };
                    UiAnswer::Confirm(flag)
                }
                QuestionKind::Select => {
                    if value.is_null() {
                        UiAnswer::Text(None)
                    } else if let Some(s) = value.as_str() {
                        if !item
                            .question
                            .options
                            .as_ref()
                            .is_some_and(|opts| opts.iter().any(|o| o == s))
                        {
                            return Err(UiRespondError::InvalidInput("选项不存在"));
                        }
                        UiAnswer::Text(Some(s.to_owned()))
                    } else {
                        return Err(UiRespondError::InvalidInput("回答格式无效"));
                    }
                }
                QuestionKind::Input | QuestionKind::Editor => {
                    if value.is_null() {
                        UiAnswer::Text(None)
                    } else if let Some(s) = value.as_str() {
                        UiAnswer::Text(Some(s.to_owned()))
                    } else {
                        return Err(UiRespondError::InvalidInput("回答格式无效"));
                    }
                }
            };
            let removed = guard.pending.remove(request_id).expect("verified above");
            let snapshot: Vec<Question> =
                guard.pending.values().map(|i| i.question.clone()).collect();
            let listener = guard.on_change.clone();
            (removed.sender, parsed, snapshot, listener)
        };
        let _ = sender.send(answer);
        if let Some(cb) = listener {
            cb(snapshot);
        }
        Ok(())
    }

    pub fn close(&self) {
        let (items, listener) = {
            let mut guard = self.inner.lock().expect("ui bridge lock");
            guard.closed = true;
            let items: Vec<PendingUiItem> =
                std::mem::take(&mut guard.pending).into_values().collect();
            let listener = guard.on_change.clone();
            (items, listener)
        };
        for item in items {
            let default = match item.question.kind {
                QuestionKind::Confirm => UiAnswer::Confirm(false),
                _ => UiAnswer::Text(None),
            };
            let _ = item.sender.send(default);
        }
        if let Some(cb) = listener {
            cb(Vec::new());
        }
    }

    async fn ask_internal(
        &self,
        question_without_id: Question,
        timeout_ms: Option<u64>,
        cancel: CancellationToken,
    ) -> Option<UiAnswer> {
        if cancel.is_cancelled() {
            return None;
        }
        let request_id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        let mut question = question_without_id;
        question.request_id = request_id.clone();

        let (snapshot, listener) = {
            let mut guard = self.inner.lock().expect("ui bridge lock");
            if guard.closed {
                return None;
            }
            guard.pending.insert(
                request_id.clone(),
                PendingUiItem {
                    question,
                    sender: tx,
                },
            );
            let snapshot: Vec<Question> =
                guard.pending.values().map(|i| i.question.clone()).collect();
            (snapshot, guard.on_change.clone())
        };
        if let Some(cb) = listener {
            cb(snapshot);
        }

        let wait_fut = async {
            match timeout_ms.filter(|ms| *ms > 0) {
                Some(ms) => tokio::select! {
                    biased;
                    _ = cancel.cancelled() => None,
                    res = tokio::time::timeout(Duration::from_millis(ms), rx) => res.ok().and_then(Result::ok),
                },
                None => tokio::select! {
                    biased;
                    _ = cancel.cancelled() => None,
                    res = rx => res.ok(),
                },
            }
        };

        let out = wait_fut.await;
        if out.is_none() {
            let removed = {
                let mut guard = self.inner.lock().expect("ui bridge lock");
                let r = guard.pending.remove(&request_id).is_some();
                let snapshot: Vec<Question> =
                    guard.pending.values().map(|i| i.question.clone()).collect();
                (r, snapshot, guard.on_change.clone())
            };
            if removed.0
                && let Some(cb) = removed.2
            {
                cb(removed.1);
            }
        }
        out
    }
}

impl ExtensionUiContext for ServerUiBridge {
    fn has_ui(&self) -> bool {
        !self.inner.lock().expect("ui bridge lock").closed
    }

    fn select<'a>(
        &'a self,
        title: &'a str,
        options: &'a [String],
        opts: Option<UiDialogOptions>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Option<String>> {
        let bridge = self.clone();
        let title = title.to_owned();
        let options = options.to_vec();
        let timeout_ms = opts.and_then(|o| o.timeout_ms);
        Box::pin(async move {
            let q = Question {
                request_id: String::new(),
                kind: QuestionKind::Select,
                title,
                options: Some(options),
                message: None,
                prefill: None,
            };
            match bridge.ask_internal(q, timeout_ms, cancel).await {
                Some(UiAnswer::Text(val)) => val,
                _ => None,
            }
        })
    }

    fn confirm<'a>(
        &'a self,
        title: &'a str,
        message: &'a str,
        opts: Option<UiDialogOptions>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, bool> {
        let bridge = self.clone();
        let title = title.to_owned();
        let message = message.to_owned();
        let timeout_ms = opts.and_then(|o| o.timeout_ms);
        Box::pin(async move {
            let q = Question {
                request_id: String::new(),
                kind: QuestionKind::Confirm,
                title,
                options: None,
                message: Some(message),
                prefill: None,
            };
            matches!(
                bridge.ask_internal(q, timeout_ms, cancel).await,
                Some(UiAnswer::Confirm(true))
            )
        })
    }

    fn input<'a>(
        &'a self,
        title: &'a str,
        placeholder: Option<&'a str>,
        opts: Option<UiDialogOptions>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Option<String>> {
        // Hard secret isolation: reject any `secret == true` prompt from entering `ui: Question[]`
        // or the idempotent `/api/sessions/:id/ui/:requestId/response` channel.
        if opts.as_ref().is_some_and(|o| o.secret) {
            return Box::pin(async { None });
        }
        let bridge = self.clone();
        let title = title.to_owned();
        let prefill = placeholder.map(str::to_owned);
        let timeout_ms = opts.and_then(|o| o.timeout_ms);
        Box::pin(async move {
            let q = Question {
                request_id: String::new(),
                kind: QuestionKind::Input,
                title,
                options: None,
                message: None,
                prefill,
            };
            match bridge.ask_internal(q, timeout_ms, cancel).await {
                Some(UiAnswer::Text(val)) => val,
                _ => None,
            }
        })
    }

    fn editor<'a>(
        &'a self,
        title: &'a str,
        prefill: Option<&'a str>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Option<String>> {
        let bridge = self.clone();
        let title = title.to_owned();
        let prefill = prefill.map(str::to_owned);
        Box::pin(async move {
            let q = Question {
                request_id: String::new(),
                kind: QuestionKind::Editor,
                title,
                options: None,
                message: None,
                prefill,
            };
            match bridge.ask_internal(q, None, cancel).await {
                Some(UiAnswer::Text(val)) => val,
                _ => None,
            }
        })
    }

    fn notify(&self, _message: &str, _level: UiNotifyLevel) {}
    fn set_status(&self, _key: &str, _text: Option<&str>) {}
    fn set_working_message(&self, _message: Option<&str>) {}
    fn set_working_visible(&self, _visible: bool) {}
    fn set_working_indicator(&self, _options: Option<WorkingIndicatorOptions>) {}
    fn set_hidden_thinking_label(&self, _label: Option<&str>) {}
    fn set_title(&self, _title: &str) {}
    fn paste_to_editor(&self, _text: &str) {}
    fn set_editor_text(&self, _text: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
}

// =============================================================================
// 2. Structured BitLocker Session Bridge (`BitLockerSessionBridge`)
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BitLockerPendingRequest {
    pub request_id: String,
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    pub kind: BitLockerPendingKind,
    pub expires_at: u64,
    pub targets: Vec<Volume>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BitLockerRequestsSnapshot {
    pub requests: Vec<BitLockerPendingRequest>,
    pub revision: u64,
    pub epoch: String,
}

struct PendingBitLockerItem {
    request: BitLockerPendingRequest,
    sender: oneshot::Sender<BitLockerAnswer>,
}

struct BitLockerBridgeInner {
    closed: bool,
    revision: u64,
    pending: BTreeMap<String, PendingBitLockerItem>,
    on_change: Option<Arc<dyn Fn(BitLockerRequestsSnapshot) + Send + Sync>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BitLockerRespondError {
    Expired(&'static str),
    InvalidInput(&'static str),
}

impl BitLockerRespondError {
    pub fn status_code(&self) -> u16 {
        match self {
            Self::Expired(_) => 409,
            Self::InvalidInput(_) => 400,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Expired(_) => "expired_request",
            Self::InvalidInput(_) => "invalid_bitlocker_input",
        }
    }

    pub fn message(&self) -> &'static str {
        match self {
            Self::Expired(msg) | Self::InvalidInput(msg) => msg,
        }
    }
}

#[derive(Clone)]
pub struct BitLockerSessionBridge {
    epoch: String,
    default_timeout: Duration,
    session: Arc<OnceLock<AgentSession>>,
    inner: Arc<Mutex<BitLockerBridgeInner>>,
}

impl Default for BitLockerSessionBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl BitLockerSessionBridge {
    pub fn new() -> Self {
        Self::with_timeout(Duration::from_secs(180))
    }

    pub fn with_timeout(default_timeout: Duration) -> Self {
        Self {
            epoch: Uuid::new_v4().to_string(),
            default_timeout,
            session: Arc::new(OnceLock::new()),
            inner: Arc::new(Mutex::new(BitLockerBridgeInner {
                closed: false,
                revision: 0,
                pending: BTreeMap::new(),
                on_change: None,
            })),
        }
    }

    pub fn bind_session(&self, session: AgentSession) {
        let _ = self.session.set(session);
    }

    pub fn set_on_change(
        &self,
        listener: impl Fn(BitLockerRequestsSnapshot) + Send + Sync + 'static,
    ) {
        self.inner.lock().expect("bitlocker bridge lock").on_change = Some(Arc::new(listener));
    }

    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    pub fn snapshot(&self) -> BitLockerRequestsSnapshot {
        let guard = self.inner.lock().expect("bitlocker bridge lock");
        Self::snapshot_locked(&self.epoch, &guard)
    }

    pub fn has_pending(&self) -> bool {
        !self
            .inner
            .lock()
            .expect("bitlocker bridge lock")
            .pending
            .is_empty()
    }

    fn snapshot_locked(epoch: &str, guard: &BitLockerBridgeInner) -> BitLockerRequestsSnapshot {
        BitLockerRequestsSnapshot {
            requests: guard.pending.values().map(|i| i.request.clone()).collect(),
            revision: guard.revision,
            epoch: epoch.to_owned(),
        }
    }

    fn find_entry_id_for_tool_call(&self, tool_call_id: &str) -> Option<String> {
        let session = self.session.get()?;
        session.with_session_manager(|sm| {
            sm.get_branch(None).into_iter().rev().find_map(|entry| {
                match entry {
                    SessionEntry::Message(msg_entry) => match &msg_entry.message {
                        AgentMessage::Assistant(assistant)
                            if assistant.content.iter().any(|part| {
                                matches!(part, ContentBlock::ToolCall(tc) if tc.id == tool_call_id)
                            }) =>
                        {
                            Some(msg_entry.id)
                        }
                        _ => None,
                    },
                    _ => None,
                }
            })
        })
    }

    /// Settles a pending BitLocker prompt using the strict validate-before-consume protocol:
    /// 1. Lookup `request_id` (409 `expired_request` if missing).
    /// 2. Check `now >= expires_at` (if expired, remove, increment revision, broadcast, return 409 `expired_request`).
    /// 3. Validate single action field and password format BEFORE removing (return 400 `invalid_bitlocker_input` on error while keeping `request_id` alive for retry).
    /// 4. Atomically remove `request_id` under the same `Mutex` lock, increment revision, wake `oneshot::Sender`, and broadcast.
    pub fn respond(&self, request_id: &str, body: &Value) -> Result<(), BitLockerRespondError> {
        let now = now_ms();
        let notify_outside;
        let result = {
            let mut guard = self.inner.lock().expect("bitlocker bridge lock");
            let Some(existing) = guard.pending.get(request_id) else {
                return Err(BitLockerRespondError::Expired("请求已处理或失效"));
            };

            if now >= existing.request.expires_at {
                guard.pending.remove(request_id);
                guard.revision += 1;
                let snap = Self::snapshot_locked(&self.epoch, &guard);
                notify_outside = guard.on_change.clone().map(|cb| (cb, snap));
                Err(BitLockerRespondError::Expired("请求已处理或失效"))
            } else {
                let Some(obj) = body.as_object() else {
                    return Err(BitLockerRespondError::InvalidInput(
                        "请只提交密码、确认或取消",
                    ));
                };
                if obj.len() != 1 {
                    return Err(BitLockerRespondError::InvalidInput(
                        "请只提交密码、确认或取消",
                    ));
                }

                let answer = if obj.get("cancel") == Some(&Value::Bool(true)) {
                    BitLockerAnswer::Cancelled
                } else {
                    match existing.request.kind {
                        BitLockerPendingKind::Password => {
                            let Some(raw_pw) = obj.get("password").and_then(Value::as_str) else {
                                return Err(BitLockerRespondError::InvalidInput("请提供恢复密码"));
                            };
                            let validated =
                                validate_recovery_password(Zeroizing::new(raw_pw.to_owned()))
                                    .map_err(|_| {
                                        BitLockerRespondError::InvalidInput(
                                            "恢复密码必须为八组六位数字，用连字符分隔",
                                        )
                                    })?;
                            BitLockerAnswer::Password(validated)
                        }
                        BitLockerPendingKind::Parameter | BitLockerPendingKind::Decrypt => {
                            let Some(confirm) = obj.get("confirm").and_then(Value::as_bool) else {
                                return Err(BitLockerRespondError::InvalidInput(
                                    "请明确确认或取消",
                                ));
                            };
                            BitLockerAnswer::Confirm(confirm)
                        }
                    }
                };

                let item = guard
                    .pending
                    .remove(request_id)
                    .expect("verified in same lock scope");
                guard.revision += 1;
                let snap = Self::snapshot_locked(&self.epoch, &guard);
                notify_outside = guard.on_change.clone().map(|cb| (cb, snap));
                Ok((item.sender, answer))
            }
        };

        if let Some((cb, snap)) = notify_outside {
            cb(snap);
        }

        match result {
            Ok((sender, answer)) => {
                let _ = sender.send(answer);
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    pub fn close(&self) {
        let notify_outside = {
            let mut guard = self.inner.lock().expect("bitlocker bridge lock");
            guard.closed = true;
            if guard.pending.is_empty() {
                None
            } else {
                guard.pending.clear();
                guard.revision += 1;
                let snap = Self::snapshot_locked(&self.epoch, &guard);
                guard.on_change.clone().map(|cb| (cb, snap))
            }
        };
        if let Some((cb, snap)) = notify_outside {
            cb(snap);
        }
    }

    pub fn to_sse_event_value(snapshot: &BitLockerRequestsSnapshot) -> Value {
        json!({
            "type": "bitlocker_requests_changed",
            "requests": snapshot.requests,
            "revision": snapshot.revision,
            "epoch": snapshot.epoch,
        })
    }
}

impl BitLockerHostChannel for BitLockerSessionBridge {
    fn is_available(&self) -> bool {
        !self.inner.lock().expect("bitlocker bridge lock").closed
    }

    fn request(
        &self,
        tool_call_id: &str,
        kind: BitLockerPendingKind,
        targets: Vec<Volume>,
        timeout: Duration,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Option<BitLockerAnswer>> {
        let bridge = self.clone();
        let tool_call_id = tool_call_id.to_owned();
        let effective_timeout = timeout.min(self.default_timeout);
        Box::pin(async move {
            if cancel.is_cancelled() {
                return None;
            }
            let request_id = Uuid::new_v4().to_string();
            let entry_id = bridge.find_entry_id_for_tool_call(&tool_call_id);
            let expires_at = now_ms()
                .saturating_add(u64::try_from(effective_timeout.as_millis()).unwrap_or(180_000));
            let (tx, rx) = oneshot::channel();

            let notify_outside = {
                let mut guard = bridge.inner.lock().expect("bitlocker bridge lock");
                if guard.closed {
                    return None;
                }
                guard.pending.insert(
                    request_id.clone(),
                    PendingBitLockerItem {
                        request: BitLockerPendingRequest {
                            request_id: request_id.clone(),
                            tool_call_id,
                            entry_id,
                            kind,
                            expires_at,
                            targets,
                        },
                        sender: tx,
                    },
                );
                guard.revision += 1;
                let snap = Self::snapshot_locked(&bridge.epoch, &guard);
                guard.on_change.clone().map(|cb| (cb, snap))
            };
            if let Some((cb, snap)) = notify_outside {
                cb(snap);
            }

            let outcome = tokio::select! {
                biased;
                _ = cancel.cancelled() => None,
                res = tokio::time::timeout(effective_timeout, rx) => res.ok().and_then(Result::ok),
            };

            if outcome.is_none() {
                let notify_cleanup = {
                    let mut guard = bridge.inner.lock().expect("bitlocker bridge lock");
                    if guard.pending.remove(&request_id).is_some() {
                        guard.revision += 1;
                        let snap = Self::snapshot_locked(&bridge.epoch, &guard);
                        guard.on_change.clone().map(|cb| (cb, snap))
                    } else {
                        None
                    }
                };
                if let Some((cb, snap)) = notify_cleanup {
                    cb(snap);
                }
            }

            match outcome {
                Some(BitLockerAnswer::Cancelled) => None,
                other => other,
            }
        })
    }
}

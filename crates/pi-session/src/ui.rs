use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_agent::BoxFuture;
use pi_ai::JsonValue;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiPromptKind {
    Select,
    Confirm,
    Input,
    Editor,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiNotifyLevel {
    #[default]
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtensionMode {
    Interactive,
    Rpc,
    #[default]
    Print,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiDialogOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<String>,
    /// 敏感输入标记（如 BitLocker 恢复密码），开启后前端隐藏回显且后端绝不写入持久化日志。
    #[serde(default)]
    pub secret: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkingIndicatorOptions {
    pub frames: Option<Vec<String>>,
    pub interval_ms: Option<u64>,
}

/// 与具体界面（TUI / Web / GPUI）无关的通用交互上下文接口（对照 `ExtensionUIContext`）。
pub trait ExtensionUiContext: Send + Sync {
    fn has_ui(&self) -> bool;

    fn select<'a>(
        &'a self,
        title: &'a str,
        options: &'a [String],
        opts: Option<UiDialogOptions>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Option<String>>;

    fn confirm<'a>(
        &'a self,
        title: &'a str,
        message: &'a str,
        opts: Option<UiDialogOptions>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, bool>;

    fn input<'a>(
        &'a self,
        title: &'a str,
        placeholder: Option<&'a str>,
        opts: Option<UiDialogOptions>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Option<String>>;

    fn editor<'a>(
        &'a self,
        title: &'a str,
        prefill: Option<&'a str>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Option<String>>;

    fn custom<'a>(
        &'a self,
        payload: JsonValue,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Option<JsonValue>> {
        let _ = (payload, cancel);
        Box::pin(async { None })
    }

    fn notify(&self, message: &str, level: UiNotifyLevel);
    fn set_status(&self, key: &str, text: Option<&str>);
    fn set_working_message(&self, message: Option<&str>);
    fn set_working_visible(&self, visible: bool);
    fn set_working_indicator(&self, options: Option<WorkingIndicatorOptions>);
    fn set_hidden_thinking_label(&self, label: Option<&str>);
    fn set_title(&self, title: &str);
    fn paste_to_editor(&self, text: &str);
    fn set_editor_text(&self, text: &str);
    fn get_editor_text(&self) -> String;
}

/// 默认的无头空实现（`has_ui() == false`，所有阻塞弹窗立即返回默认值）。
#[derive(Debug, Clone, Default)]
pub struct NoOpUiContext {
    editor_text: Arc<Mutex<String>>,
}

impl NoOpUiContext {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ExtensionUiContext for NoOpUiContext {
    fn has_ui(&self) -> bool {
        false
    }

    fn select<'a>(
        &'a self,
        _title: &'a str,
        _options: &'a [String],
        _opts: Option<UiDialogOptions>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, Option<String>> {
        Box::pin(async { None })
    }

    fn confirm<'a>(
        &'a self,
        _title: &'a str,
        _message: &'a str,
        _opts: Option<UiDialogOptions>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }

    fn input<'a>(
        &'a self,
        _title: &'a str,
        _placeholder: Option<&'a str>,
        _opts: Option<UiDialogOptions>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, Option<String>> {
        Box::pin(async { None })
    }

    fn editor<'a>(
        &'a self,
        _title: &'a str,
        _prefill: Option<&'a str>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, Option<String>> {
        Box::pin(async { None })
    }

    fn notify(&self, _message: &str, _level: UiNotifyLevel) {}
    fn set_status(&self, _key: &str, _text: Option<&str>) {}
    fn set_working_message(&self, _message: Option<&str>) {}
    fn set_working_visible(&self, _visible: bool) {}
    fn set_working_indicator(&self, _options: Option<WorkingIndicatorOptions>) {}
    fn set_hidden_thinking_label(&self, _label: Option<&str>) {}
    fn set_title(&self, _title: &str) {}

    fn paste_to_editor(&self, text: &str) {
        if let Ok(mut guard) = self.editor_text.lock() {
            guard.push_str(text);
        }
    }

    fn set_editor_text(&self, text: &str) {
        if let Ok(mut guard) = self.editor_text.lock() {
            *guard = text.to_owned();
        }
    }

    fn get_editor_text(&self) -> String {
        self.editor_text
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }
}

// =============================================================================
// ChannelUiContext (Async Request/Response Bridge for TUI / Web / GPUI / Tests)
// =============================================================================

#[derive(Debug)]
pub enum UiPromptRequest {
    Select {
        title: String,
        options: Vec<String>,
        opts: Option<UiDialogOptions>,
        respond: oneshot::Sender<Option<String>>,
    },
    Confirm {
        title: String,
        message: String,
        opts: Option<UiDialogOptions>,
        respond: oneshot::Sender<bool>,
    },
    Input {
        title: String,
        placeholder: Option<String>,
        opts: Option<UiDialogOptions>,
        respond: oneshot::Sender<Option<String>>,
    },
    Editor {
        title: String,
        prefill: Option<String>,
        respond: oneshot::Sender<Option<String>>,
    },
    Custom {
        payload: JsonValue,
        respond: oneshot::Sender<Option<JsonValue>>,
    },
}

#[derive(Debug, Clone, Default)]
pub struct ChannelUiStateSnapshot {
    pub notifications: Vec<(String, UiNotifyLevel)>,
    pub statuses: BTreeMap<String, String>,
    pub working_message: Option<String>,
    pub working_visible: bool,
    pub working_indicator: Option<WorkingIndicatorOptions>,
    pub hidden_thinking_label: Option<String>,
    pub title: Option<String>,
    pub editor_text: String,
}

#[derive(Clone)]
pub struct ChannelUiContext {
    tx: mpsc::UnboundedSender<UiPromptRequest>,
    state: Arc<Mutex<ChannelUiStateSnapshot>>,
}

impl ChannelUiContext {
    pub fn pair() -> (Self, mpsc::UnboundedReceiver<UiPromptRequest>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                tx,
                state: Arc::new(Mutex::new(ChannelUiStateSnapshot {
                    working_visible: true,
                    ..ChannelUiStateSnapshot::default()
                })),
            },
            rx,
        )
    }

    pub fn snapshot(&self) -> ChannelUiStateSnapshot {
        self.state.lock().expect("channel ui state lock").clone()
    }
}

async fn await_ui_response<T>(
    rx: oneshot::Receiver<T>,
    timeout_ms: Option<u64>,
    cancel: CancellationToken,
    default_on_cancel: T,
) -> T {
    if cancel.is_cancelled() {
        return default_on_cancel;
    }
    if let Some(ms) = timeout_ms {
        tokio::select! {
            res = rx => res.unwrap_or(default_on_cancel),
            _ = cancel.cancelled() => default_on_cancel,
            _ = tokio::time::sleep(Duration::from_millis(ms)) => default_on_cancel,
        }
    } else {
        tokio::select! {
            res = rx => res.unwrap_or(default_on_cancel),
            _ = cancel.cancelled() => default_on_cancel,
        }
    }
}

impl ExtensionUiContext for ChannelUiContext {
    fn has_ui(&self) -> bool {
        true
    }

    fn select<'a>(
        &'a self,
        title: &'a str,
        options: &'a [String],
        opts: Option<UiDialogOptions>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Option<String>> {
        let tx = self.tx.clone();
        let title_s = title.to_owned();
        let options_v = options.to_vec();
        let timeout_ms = opts.as_ref().and_then(|o| o.timeout_ms);
        Box::pin(async move {
            let (resp_tx, resp_rx) = oneshot::channel();
            if tx
                .send(UiPromptRequest::Select {
                    title: title_s,
                    options: options_v,
                    opts,
                    respond: resp_tx,
                })
                .is_err()
            {
                return None;
            }
            await_ui_response(resp_rx, timeout_ms, cancel, None).await
        })
    }

    fn confirm<'a>(
        &'a self,
        title: &'a str,
        message: &'a str,
        opts: Option<UiDialogOptions>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, bool> {
        let tx = self.tx.clone();
        let title_s = title.to_owned();
        let msg_s = message.to_owned();
        let timeout_ms = opts.as_ref().and_then(|o| o.timeout_ms);
        Box::pin(async move {
            let (resp_tx, resp_rx) = oneshot::channel();
            if tx
                .send(UiPromptRequest::Confirm {
                    title: title_s,
                    message: msg_s,
                    opts,
                    respond: resp_tx,
                })
                .is_err()
            {
                return false;
            }
            await_ui_response(resp_rx, timeout_ms, cancel, false).await
        })
    }

    fn input<'a>(
        &'a self,
        title: &'a str,
        placeholder: Option<&'a str>,
        opts: Option<UiDialogOptions>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Option<String>> {
        let tx = self.tx.clone();
        let title_s = title.to_owned();
        let placeholder_s = placeholder.map(ToOwned::to_owned);
        let timeout_ms = opts.as_ref().and_then(|o| o.timeout_ms);
        Box::pin(async move {
            let (resp_tx, resp_rx) = oneshot::channel();
            if tx
                .send(UiPromptRequest::Input {
                    title: title_s,
                    placeholder: placeholder_s,
                    opts,
                    respond: resp_tx,
                })
                .is_err()
            {
                return None;
            }
            await_ui_response(resp_rx, timeout_ms, cancel, None).await
        })
    }

    fn editor<'a>(
        &'a self,
        title: &'a str,
        prefill: Option<&'a str>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Option<String>> {
        let tx = self.tx.clone();
        let title_s = title.to_owned();
        let prefill_s = prefill.map(ToOwned::to_owned);
        Box::pin(async move {
            let (resp_tx, resp_rx) = oneshot::channel();
            if tx
                .send(UiPromptRequest::Editor {
                    title: title_s,
                    prefill: prefill_s,
                    respond: resp_tx,
                })
                .is_err()
            {
                return None;
            }
            await_ui_response(resp_rx, None, cancel, None).await
        })
    }

    fn custom<'a>(
        &'a self,
        payload: JsonValue,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Option<JsonValue>> {
        let tx = self.tx.clone();
        Box::pin(async move {
            let (resp_tx, resp_rx) = oneshot::channel();
            if tx
                .send(UiPromptRequest::Custom {
                    payload,
                    respond: resp_tx,
                })
                .is_err()
            {
                return None;
            }
            await_ui_response(resp_rx, None, cancel, None).await
        })
    }

    fn notify(&self, message: &str, level: UiNotifyLevel) {
        if let Ok(mut guard) = self.state.lock() {
            guard.notifications.push((message.to_owned(), level));
        }
    }

    fn set_status(&self, key: &str, text: Option<&str>) {
        if let Ok(mut guard) = self.state.lock() {
            match text {
                Some(t) => {
                    guard.statuses.insert(key.to_owned(), t.to_owned());
                }
                None => {
                    guard.statuses.remove(key);
                }
            }
        }
    }

    fn set_working_message(&self, message: Option<&str>) {
        if let Ok(mut guard) = self.state.lock() {
            guard.working_message = message.map(ToOwned::to_owned);
        }
    }

    fn set_working_visible(&self, visible: bool) {
        if let Ok(mut guard) = self.state.lock() {
            guard.working_visible = visible;
        }
    }

    fn set_working_indicator(&self, options: Option<WorkingIndicatorOptions>) {
        if let Ok(mut guard) = self.state.lock() {
            guard.working_indicator = options;
        }
    }

    fn set_hidden_thinking_label(&self, label: Option<&str>) {
        if let Ok(mut guard) = self.state.lock() {
            guard.hidden_thinking_label = label.map(ToOwned::to_owned);
        }
    }

    fn set_title(&self, title: &str) {
        if let Ok(mut guard) = self.state.lock() {
            guard.title = Some(title.to_owned());
        }
    }

    fn paste_to_editor(&self, text: &str) {
        if let Ok(mut guard) = self.state.lock() {
            guard.editor_text.push_str(text);
        }
    }

    fn set_editor_text(&self, text: &str) {
        if let Ok(mut guard) = self.state.lock() {
            guard.editor_text = text.to_owned();
        }
    }

    fn get_editor_text(&self) -> String {
        self.state
            .lock()
            .map(|g| g.editor_text.clone())
            .unwrap_or_default()
    }
}

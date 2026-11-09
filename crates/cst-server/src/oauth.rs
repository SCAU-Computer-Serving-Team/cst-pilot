use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pi_agent::BoxFuture;
use pi_ai::{AuthEvent, AuthInteraction, AuthPrompt, AuthType};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Notify, oneshot};
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::error::{ServiceError, ServiceErrorKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCodeView {
    pub user_code: String,
    pub verification_uri: String,
    pub interval_seconds: u64,
    pub expires_in_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthUrlView {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthLinkView {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthFlowView {
    pub auth_type: String,
    pub flow_id: String,
    pub revision: u64,
    pub state: String, // "pending" | "succeeded" | "failed" | "cancelled"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_code: Option<DeviceCodeView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<AuthUrlView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<AuthLinkView>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

struct PendingPrompt {
    prompt_id: String,
    prompt_kind: AuthPrompt,
    sender: oneshot::Sender<Result<String, String>>,
}

pub struct ActiveFlow {
    view: OAuthFlowView,
    cancel: CancellationToken,
    device_expires_at: Option<Instant>,
    pending_prompt: Option<PendingPrompt>,
    change_notify: Arc<Notify>,
    done_notify: Arc<Notify>,
    finished: bool,
}

type FlowChangeCallback = Arc<dyn Fn(&str, &OAuthFlowView) + Send + Sync>;

#[derive(Clone)]
pub struct OAuthFlowManager {
    flows: Arc<Mutex<HashMap<String, Arc<Mutex<ActiveFlow>>>>>,
    on_change: FlowChangeCallback,
}

pub(crate) fn auth_type_str(auth_type: AuthType) -> &'static str {
    match auth_type {
        AuthType::OAuth => "oauth",
        AuthType::ApiKey => "api_key",
    }
}

impl OAuthFlowManager {
    pub fn new(on_change: impl Fn(&str, &OAuthFlowView) + Send + Sync + 'static) -> Self {
        Self {
            flows: Arc::new(Mutex::new(HashMap::new())),
            on_change: Arc::new(on_change),
        }
    }

    pub fn status(&self, provider_id: &str, expected_type: Option<&str>) -> Value {
        let Some(flow_arc) = self
            .flows
            .lock()
            .expect("flows lock")
            .get(provider_id)
            .cloned()
        else {
            return json!({ "state": "idle" });
        };
        let guard = flow_arc.lock().expect("flow lock");
        if let Some(exp) = expected_type
            && guard.view.state != "idle"
            && guard.view.auth_type != exp
        {
            return json!({ "state": "idle" });
        }
        let mut view = guard.view.clone();
        if let (Some(dc), Some(exp_at)) = (&mut view.device_code, guard.device_expires_at) {
            let rem = exp_at
                .saturating_duration_since(Instant::now())
                .as_secs_f64()
                .ceil() as u64;
            dc.expires_in_seconds = rem;
        }
        serde_json::to_value(view).unwrap_or_else(|_| json!({ "state": "idle" }))
    }

    pub fn start_or_reuse(
        &self,
        provider_id: &str,
        auth_type: AuthType,
        initial_secret: Option<Zeroizing<String>>,
        run_login: impl FnOnce(AuthInteraction) -> BoxFuture<'static, Result<(), String>>
        + Send
        + 'static,
    ) -> Arc<Mutex<ActiveFlow>> {
        let auth_type_str = auth_type_str(auth_type).to_owned();
        {
            let map = self.flows.lock().expect("flows lock");
            if let Some(existing) = map.get(provider_id).cloned()
                && existing.lock().expect("flow lock").view.state == "pending"
            {
                return existing;
            }
        }

        let flow_id = Uuid::new_v4().to_string();
        let cancel = CancellationToken::new();
        let change_notify = Arc::new(Notify::new());
        let done_notify = Arc::new(Notify::new());

        let flow_arc = Arc::new(Mutex::new(ActiveFlow {
            view: OAuthFlowView {
                auth_type: auth_type_str,
                flow_id: flow_id.clone(),
                revision: 0,
                state: "pending".to_owned(),
                device_code: None,
                auth_url: None,
                prompt: None,
                message: None,
                links: None,
                error: None,
            },
            cancel: cancel.clone(),
            device_expires_at: None,
            pending_prompt: None,
            change_notify: change_notify.clone(),
            done_notify: done_notify.clone(),
            finished: false,
        }));

        self.flows
            .lock()
            .expect("flows lock")
            .insert(provider_id.to_owned(), flow_arc.clone());

        let first_secret = Arc::new(Mutex::new(initial_secret));
        let flow_for_prompt = flow_arc.clone();
        let manager_for_prompt = self.clone();
        let pid_for_prompt = provider_id.to_owned();

        let prompt_fn: pi_ai::AuthPromptFn = Arc::new(move |question: AuthPrompt| {
            if matches!(question, AuthPrompt::Secret { .. })
                && let Some(secret) = first_secret.lock().expect("secret lock").take()
            {
                let val = secret.as_str().to_owned();
                return Box::pin(async move { Ok(val) });
            }
            let flow = flow_for_prompt.clone();
            let manager = manager_for_prompt.clone();
            let pid = pid_for_prompt.clone();
            Box::pin(async move { manager.wait_for_prompt(&pid, &flow, question).await })
        });

        let flow_for_notify = flow_arc.clone();
        let manager_for_notify = self.clone();
        let pid_for_notify = provider_id.to_owned();

        let notify_fn: pi_ai::AuthNotifyFn = Arc::new(move |event: AuthEvent| {
            manager_for_notify.on_auth_event(&pid_for_notify, &flow_for_notify, event);
        });

        let interaction = AuthInteraction {
            cancel_token: Some(cancel.clone()),
            prompt: prompt_fn,
            notify: notify_fn,
        };

        let flow_for_task = flow_arc.clone();
        let manager_for_task = self.clone();
        let pid_for_task = provider_id.to_owned();

        tokio::spawn(async move {
            let timeout_res =
                tokio::time::timeout(Duration::from_secs(15 * 60), run_login(interaction)).await;
            let notify_view = {
                let mut guard = flow_for_task.lock().expect("flow lock");
                guard.pending_prompt = None;
                guard.view.prompt = None;
                guard.view.auth_url = None;
                guard.view.device_code = None;
                guard.view.links = None;
                guard.finished = true;

                if guard.cancel.is_cancelled() {
                    guard.view.state = "cancelled".to_owned();
                } else {
                    match timeout_res {
                        Ok(Ok(())) => {
                            guard.view.state = "succeeded".to_owned();
                        }
                        Ok(Err(err)) => {
                            guard.view.state = "failed".to_owned();
                            guard.view.error = Some(if err.is_empty() {
                                "登录未完成，请重试。".to_owned()
                            } else {
                                err
                            });
                        }
                        Err(_) => {
                            guard.cancel.cancel();
                            guard.view.state = "cancelled".to_owned();
                        }
                    }
                }
                guard.view.revision += 1;
                guard.change_notify.notify_waiters();
                guard.done_notify.notify_waiters();
                guard.view.clone()
            };
            (manager_for_task.on_change)(&pid_for_task, &notify_view);
        });

        flow_arc
    }

    async fn wait_for_prompt(
        &self,
        provider_id: &str,
        flow: &Arc<Mutex<ActiveFlow>>,
        question: AuthPrompt,
    ) -> Result<String, String> {
        let (rx, cancel, view_snapshot) = {
            let mut guard = flow.lock().expect("flow lock");
            if guard.cancel.is_cancelled() {
                return Err("Login cancelled".to_owned());
            }
            if guard.pending_prompt.is_some() {
                return Err("请求的授权步骤仍在等待回答".to_owned());
            }
            let prompt_id = Uuid::new_v4().to_string();
            let mut prompt_json = serde_json::to_value(&question).unwrap_or_else(|_| json!({}));
            if let Some(obj) = prompt_json.as_object_mut() {
                obj.insert("id".to_owned(), json!(prompt_id));
            }
            let (tx, rx) = oneshot::channel();
            guard.view.prompt = Some(prompt_json);
            guard.view.revision += 1;
            guard.pending_prompt = Some(PendingPrompt {
                prompt_id,
                prompt_kind: question,
                sender: tx,
            });
            guard.change_notify.notify_waiters();
            (rx, guard.cancel.clone(), guard.view.clone())
        };
        (self.on_change)(provider_id, &view_snapshot);

        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err("Login cancelled".to_owned()),
            res = rx => res.map_err(|_| "Login cancelled".to_owned())?,
        }
    }

    fn on_auth_event(&self, provider_id: &str, flow: &Arc<Mutex<ActiveFlow>>, event: AuthEvent) {
        let view_snapshot = {
            let mut guard = flow.lock().expect("flow lock");
            if guard.cancel.is_cancelled() || guard.finished {
                return;
            }
            match event {
                AuthEvent::DeviceCode {
                    user_code,
                    verification_uri,
                    interval_seconds,
                    expires_in_seconds,
                    ..
                } => {
                    let Some(uri) = validate_http_url(&verification_uri) else {
                        return;
                    };
                    let secs = expires_in_seconds.unwrap_or(300);
                    guard.device_expires_at = Some(Instant::now() + Duration::from_secs(secs));
                    guard.view.device_code = Some(DeviceCodeView {
                        user_code,
                        verification_uri: uri,
                        interval_seconds: interval_seconds.unwrap_or(5),
                        expires_in_seconds: secs,
                    });
                }
                AuthEvent::AuthUrl { url, instructions } => {
                    let Some(uri) = validate_http_url(&url) else {
                        return;
                    };
                    guard.view.auth_url = Some(AuthUrlView {
                        url: uri,
                        instructions,
                    });
                }
                AuthEvent::Info { message, links } => {
                    guard.view.message = Some(message);
                    guard.view.links = links.map(|list| {
                        list.into_iter()
                            .filter_map(|l| {
                                validate_http_url(&l.url).map(|url| AuthLinkView {
                                    url,
                                    label: l.label,
                                })
                            })
                            .collect()
                    });
                }
                AuthEvent::Progress { message } => {
                    guard.view.message = Some(message);
                }
            }
            guard.view.revision += 1;
            guard.change_notify.notify_waiters();
            guard.view.clone()
        };
        (self.on_change)(provider_id, &view_snapshot);
    }

    pub async fn wait_until_ready(
        &self,
        provider_id: &str,
        flow: &Arc<Mutex<ActiveFlow>>,
    ) -> Result<Value, ServiceError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let (is_ready, notify_arc) = {
                let guard = flow.lock().expect("flow lock");
                let ready = guard.view.state != "pending"
                    || guard.view.device_code.is_some()
                    || guard.view.auth_url.is_some()
                    || guard.view.prompt.is_some()
                    || guard.view.message.is_some();
                (ready, guard.change_notify.clone())
            };
            if is_ready {
                return Ok(self.status(provider_id, None));
            }
            if tokio::time::timeout_at(deadline, notify_arc.notified())
                .await
                .is_err()
            {
                self.cancel(provider_id, None);
                return Err(ServiceError::new(
                    ServiceErrorKind::InvalidInput,
                    "invalid_request",
                    "请求授权信息超时，请重试。",
                ));
            }
        }
    }

    pub fn respond(
        &self,
        provider_id: &str,
        flow_id: &str,
        prompt_id: &str,
        value: &str,
    ) -> Result<(), ServiceError> {
        let Some(flow_arc) = self
            .flows
            .lock()
            .expect("flows lock")
            .get(provider_id)
            .cloned()
        else {
            return Err(ServiceError::new(
                ServiceErrorKind::Conflict,
                "conflict",
                "授权步骤已更新，请按当前页面继续。",
            ));
        };

        let (sender, view_snapshot) = {
            let mut guard = flow_arc.lock().expect("flow lock");
            if guard.view.flow_id != flow_id
                || guard.view.state != "pending"
                || guard
                    .pending_prompt
                    .as_ref()
                    .is_none_or(|p| p.prompt_id != prompt_id)
            {
                return Err(ServiceError::new(
                    ServiceErrorKind::Conflict,
                    "conflict",
                    "授权步骤已更新，请按当前页面继续。",
                ));
            }
            let pending = guard.pending_prompt.as_ref().expect("verified");
            if value.len() > 8192 {
                return Err(ServiceError::new(
                    ServiceErrorKind::InvalidInput,
                    "invalid_request",
                    "请求的授权回答无效",
                ));
            }
            match &pending.prompt_kind {
                AuthPrompt::Text { .. } => {}
                AuthPrompt::Secret { .. } | AuthPrompt::ManualCode { .. } => {
                    if value.trim().is_empty() {
                        return Err(ServiceError::new(
                            ServiceErrorKind::InvalidInput,
                            "invalid_request",
                            "请求的授权回答无效",
                        ));
                    }
                }
                AuthPrompt::Select { options, .. } => {
                    if value.trim().is_empty() || !options.iter().any(|o| o.id == value) {
                        return Err(ServiceError::new(
                            ServiceErrorKind::InvalidInput,
                            "invalid_request",
                            "请求的授权回答无效",
                        ));
                    }
                }
            }
            let taken = guard.pending_prompt.take().expect("verified");
            guard.view.prompt = None;
            guard.view.revision += 1;
            guard.change_notify.notify_waiters();
            (taken.sender, guard.view.clone())
        };

        let _ = sender.send(Ok(value.to_owned()));
        (self.on_change)(provider_id, &view_snapshot);
        Ok(())
    }

    pub fn cancel(&self, provider_id: &str, flow_id: Option<&str>) -> bool {
        let Some(flow_arc) = self
            .flows
            .lock()
            .expect("flows lock")
            .get(provider_id)
            .cloned()
        else {
            return false;
        };
        let view_snapshot = {
            let mut guard = flow_arc.lock().expect("flow lock");
            if guard.view.state != "pending" || flow_id.is_some_and(|fid| guard.view.flow_id != fid)
            {
                return false;
            }
            guard.view.state = "cancelled".to_owned();
            guard.cancel.cancel();
            guard.pending_prompt = None;
            guard.view.prompt = None;
            guard.view.auth_url = None;
            guard.view.device_code = None;
            guard.view.revision += 1;
            guard.change_notify.notify_waiters();
            guard.view.clone()
        };
        (self.on_change)(provider_id, &view_snapshot);
        true
    }

    pub async fn cancel_and_wait(&self, provider_id: &str) {
        let Some(flow_arc) = self
            .flows
            .lock()
            .expect("flows lock")
            .get(provider_id)
            .cloned()
        else {
            return;
        };
        self.cancel(provider_id, None);
        loop {
            let (done, notify_arc) = {
                let guard = flow_arc.lock().expect("flow lock");
                (guard.finished, guard.done_notify.clone())
            };
            if done {
                break;
            }
            notify_arc.notified().await;
        }
    }

    pub async fn close_all(&self) {
        let providers: Vec<String> = self
            .flows
            .lock()
            .expect("flows lock")
            .keys()
            .cloned()
            .collect();
        for p in providers {
            self.cancel_and_wait(&p).await;
        }
    }
}

fn validate_http_url(raw: &str) -> Option<String> {
    let url = Url::parse(raw).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.to_string())
}

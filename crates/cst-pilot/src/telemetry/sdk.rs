use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_agent::AgentMessage;
use pi_session::{
    AgentSessionEvent, BoundaryEventKind, CompactionReason, ExtensionContext, ExtensionRegistrar,
    InputEventResult, InputSource, SessionBeforeEventOutput, SessionLifecycleEvent,
};
use tokio::time::Instant;

use super::{service::TelemetryService, submission::SessionSubmission};

struct Current {
    session_id: String,
    submission: Arc<SessionSubmission>,
}
/// Reused by every extension factory generation, so reload never multiplies observers.
pub(crate) struct TelemetryBinding {
    pub service: Arc<TelemetryService>,
    channel: String,
    current: Mutex<Option<Current>>,
}
impl TelemetryBinding {
    pub fn new(service: Arc<TelemetryService>, channel: &str) -> Arc<Self> {
        Arc::new(Self {
            service,
            channel: channel.into(),
            current: Mutex::default(),
        })
    }
    fn submission(&self) -> Option<Arc<SessionSubmission>> {
        self.current
            .lock()
            .expect("telemetry binding")
            .as_ref()
            .map(|c| c.submission.clone())
    }
    fn start(&self, session_id: String, reason: &str) {
        let mut current = self.current.lock().expect("telemetry binding");
        if reason == "reload" && current.as_ref().is_some_and(|c| c.session_id == session_id) {
            return;
        }
        if let Some(old) = current.take() {
            let end_reason = match reason {
                "new" | "resume" | "fork" => reason,
                _ => "crash",
            };
            drop(old.submission.finalize(
                end_reason,
                None,
                Instant::now() + Duration::from_millis(4500),
            ));
        }
        *current = Some(Current {
            submission: self.service.instance(&session_id, reason, &self.channel),
            session_id,
        });
    }
    pub fn abandon(&self) {
        if tokio::runtime::Handle::try_current().is_ok()
            && let Some(s) = self.submission()
        {
            drop(s.finalize("crash", None, Instant::now() + Duration::from_millis(4500)));
        }
    }
    pub async fn finalize(&self, reason: &str, entries: Option<u64>) {
        if let Some(submission) = self.submission() {
            let deadline = self.service.close_or_session_deadline();
            let _ =
                tokio::time::timeout_at(deadline, submission.finalize(reason, entries, deadline))
                    .await;
        }
    }
    pub fn on_session_event(&self, event: AgentSessionEvent) {
        if let Some(submission) = self.submission()
            && let AgentSessionEvent::CompactionEnd {
                reason,
                result,
                error_message,
                ..
            } = event
        {
            submission.update(|c| {
                if let Some(result) = result {
                    c.compact(result.tokens_before, reason == CompactionReason::Overflow);
                } else if error_message.is_some() {
                    c.compact_failed();
                }
            });
        }
    }
    fn activity(&self, busy: Option<bool>) {
        if let Some(s) = self.submission() {
            self.service.activity(&s.record_id, busy);
        }
    }
    pub fn register(self: &Arc<Self>, api: &mut ExtensionRegistrar) {
        let binding = self.clone();
        api.on_input(move |event, _| {
            if let Some(s) = binding.submission() {
                s.update(|c| {
                    c.input(
                        match event.source {
                            InputSource::Interactive => "interactive",
                            InputSource::Rpc => "rpc",
                            InputSource::Extension => "extension",
                        },
                        binding.service.now(),
                    )
                });
            }
            binding.activity(Some(true));
            async { Ok(Some(InputEventResult::Continue)) }
        });
        let binding = self.clone();
        api.on_before_provider_headers(Arc::new(move |_, ctx| {
            if let Some(s) = binding.submission() {
                s.update(|c| {
                    c.before_request(
                        ctx.model()
                            .as_ref()
                            .map(|m| m.provider.as_str())
                            .unwrap_or(""),
                        ctx.thinking_level().as_str(),
                        binding.service.now(),
                    )
                });
                drop(s.checkpoint());
            }
            Box::pin(async { Ok(()) })
        }));
        let binding = self.clone();
        api.on_after_provider_response(move |event, _| {
            if let Some(s) = binding.submission() {
                s.update(|c| c.after_response(event.status, &event.headers, binding.service.now()));
                drop(s.checkpoint());
            }
            binding.activity(None);
            async { Ok(()) }
        });
        let binding = self.clone();
        api.on_turn_end(move |event, ctx| {
            if let BoundaryEventKind::TurnEnd { message, .. } = event.kind
                && let Some(s) = binding.submission()
            {
                let context = ctx
                    .get_context_usage()
                    .and_then(|v| v.tokens.map(|t| (t, v.context_window)));
                s.update(|c| {
                    c.turn_end(
                        if let AgentMessage::Assistant(ref msg) = message {
                            Some(msg)
                        } else {
                            None
                        },
                        context,
                        binding.service.now(),
                    )
                });
                drop(s.checkpoint());
            }
            async { Ok(None) }
        });
        for name in [
            "session_start",
            "session_shutdown",
            "agent_start",
            "agent_settled",
            "turn_start",
            "tool_execution_start",
            "tool_execution_end",
        ] {
            let binding = self.clone();
            api.on_lifecycle(name, move |event, ctx| {
                let binding = binding.clone();
                async move {
                    binding.lifecycle(event, ctx).await;
                    Ok(SessionBeforeEventOutput::None)
                }
            });
        }
    }
    async fn lifecycle(&self, event: SessionLifecycleEvent, ctx: ExtensionContext) {
        match event {
            SessionLifecycleEvent::SessionStart { reason, .. } => {
                self.start(ctx.session_id(), &reason)
            }
            SessionLifecycleEvent::SessionShutdown { reason, .. } => {
                let process = reason == "quit"
                    && self.channel == "tui"
                    && matches!(ctx.mode(), pi_session::ExtensionMode::Interactive);
                if process {
                    self.service.begin_close();
                }
                self.finalize(
                    &reason,
                    Some(
                        pi_session::build_context_entries(&ctx.session_branch(), None, None).len()
                            as u64,
                    ),
                )
                .await;
                if process {
                    self.service.close_process(&reason).await;
                }
            }
            SessionLifecycleEvent::AgentStart => self.activity(Some(true)),
            SessionLifecycleEvent::AgentSettled { .. } => {
                self.activity(Some(false));
                if let Some(s) = self.submission() {
                    let task = s.submit();
                    tokio::spawn(async move {
                        if task.await.is_ok() {
                            s.settle_billing();
                        }
                    });
                }
            }
            SessionLifecycleEvent::TurnStart { .. } => {
                if let Some(s) = self.submission() {
                    s.update(|c| c.turn_start(self.service.now()));
                }
            }
            SessionLifecycleEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
                ..
            } => {
                if let Some(s) = self.submission() {
                    s.update(|c| {
                        c.tool_start(&tool_call_id, &tool_name, &args, self.service.now())
                    });
                }
            }
            SessionLifecycleEvent::ToolExecutionEnd {
                tool_call_id,
                result,
                is_error,
                duration_ms,
                ..
            } => {
                if let Some(s) = self.submission() {
                    s.update(|c| {
                        c.tool_end(
                            &tool_call_id,
                            &result,
                            is_error,
                            duration_ms,
                            self.service.now(),
                        )
                    });
                }
            }
            _ => (),
        }
    }
}
impl Drop for TelemetryBinding {
    fn drop(&mut self) {
        // The normal shutdown hook is awaited. A dropped host instead leaves a recoverable crash boundary.
        if let Some(current) = self.current.lock().expect("telemetry binding").take()
            && tokio::runtime::Handle::try_current().is_ok()
        {
            drop(current.submission.finalize(
                "crash",
                None,
                Instant::now() + Duration::from_millis(4500),
            ));
        }
    }
}

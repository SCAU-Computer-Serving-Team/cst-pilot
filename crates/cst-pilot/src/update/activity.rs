//! A per-kit admission gate for business writes and update handoff.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

const CLOSING: &str = "工具包正在准备升级，请等待交接完成或取消后重试。";
#[derive(Default)]
struct State {
    active: usize,
    closing: bool,
}
#[derive(Default)]
pub struct ActivityGate(Mutex<State>);
pub struct ActivityLease(Arc<ActivityGate>);
pub struct UpgradeLease(Arc<ActivityGate>);
impl ActivityGate {
    pub fn for_root(root: &Path) -> Result<Arc<Self>, String> {
        let root = root.canonicalize().map_err(|_| "工具包目录无法读取。")?;
        let key = if cfg!(windows) {
            PathBuf::from(root.to_string_lossy().to_lowercase())
        } else {
            root
        };
        static GATES: OnceLock<Mutex<BTreeMap<PathBuf, Weak<ActivityGate>>>> = OnceLock::new();
        let mut gates = GATES
            .get_or_init(Default::default)
            .lock()
            .expect("activity gates");
        if let Some(gate) = gates.get(&key).and_then(Weak::upgrade) {
            return Ok(gate);
        }
        gates.retain(|_, gate| gate.strong_count() > 0);
        let gate = Arc::new(Self::default());
        gates.insert(key, Arc::downgrade(&gate));
        Ok(gate)
    }
    pub fn enter(self: &Arc<Self>) -> Result<ActivityLease, String> {
        let mut state = self.0.lock().expect("activity gate");
        if state.closing {
            return Err(CLOSING.into());
        }
        state.active += 1;
        Ok(ActivityLease(self.clone()))
    }
    pub fn check(&self) -> Result<(), String> {
        if self.0.lock().expect("activity gate").closing {
            Err(CLOSING.into())
        } else {
            Ok(())
        }
    }
    pub fn close(self: &Arc<Self>) -> Result<UpgradeLease, String> {
        let mut state = self.0.lock().expect("activity gate");
        if state.closing {
            return Err(CLOSING.into());
        }
        if state.active != 0 || crate::bitlocker::lock::VolumeLock::active_count() != 0 {
            return Err("仍有会话、工具或配置写入任务，请结束后重试升级。".into());
        }
        state.closing = true;
        Ok(UpgradeLease(self.clone()))
    }
    pub(crate) fn wrap(self: &Arc<Self>, tools: &mut [pi_session::ToolDefinition]) {
        for tool in tools {
            let gate = self.clone();
            let execute = tool.execute.clone();
            tool.execute = Arc::new(move |id, args, cancel, update, ctx| {
                let execute = execute.clone();
                let permit = gate.enter();
                Box::pin(async move {
                    let _permit = permit?;
                    execute(id, args, cancel, update, ctx).await
                })
            });
        }
    }
    pub(crate) fn register(self: &Arc<Self>, api: &mut pi_session::ExtensionRegistrar) {
        let gate = self.clone();
        api.on_input(move |_, ctx| {
            let closed = gate.check().is_err();
            if closed {
                ctx.ui().notify(CLOSING, pi_session::UiNotifyLevel::Warning);
            }
            async move {
                Ok(Some(if closed {
                    pi_session::InputEventResult::Handled
                } else {
                    pi_session::InputEventResult::Continue
                }))
            }
        });
        let gate = self.clone();
        api.on_before_provider_headers(Arc::new(move |_, ctx| {
            if gate.check().is_err()
                && let Some(cancel) = ctx.cancel_token()
            {
                cancel.cancel();
            }
            Box::pin(async { Ok(()) })
        }));
        for name in [
            "session_before_switch",
            "session_before_fork",
            "session_before_compact",
            "session_before_tree",
        ] {
            let gate = self.clone();
            api.on_lifecycle(name, move |event, _| {
                let cancel = gate.check().is_err();
                async move {
                    use pi_session::{SessionBeforeEventOutput as O, SessionLifecycleEvent as E};
                    Ok(match event {
                        E::SessionBeforeSwitch { .. } => {
                            O::Switch(pi_session::SessionBeforeSwitchResult { cancel })
                        }
                        E::SessionBeforeFork { .. } => {
                            O::Fork(pi_session::SessionBeforeForkResult {
                                cancel,
                                ..Default::default()
                            })
                        }
                        E::SessionBeforeCompact { .. } => {
                            O::Compact(pi_session::SessionBeforeCompactResult {
                                cancel,
                                ..Default::default()
                            })
                        }
                        E::SessionBeforeTree { .. } => {
                            O::Tree(pi_session::SessionBeforeTreeResult {
                                cancel,
                                ..Default::default()
                            })
                        }
                        _ => O::None,
                    })
                }
            });
        }
    }
}
impl Drop for ActivityLease {
    fn drop(&mut self) {
        self.0.0.lock().expect("activity gate").active -= 1;
    }
}
impl Drop for UpgradeLease {
    fn drop(&mut self) {
        self.0.0.lock().expect("activity gate").closing = false;
    }
}

/// Composes admission with the fixed privacy projection; no caller can disable either.
pub(crate) struct InputPolicy(pub Arc<ActivityGate>, pub bool);
impl pi_session::SessionDataPolicy for InputPolicy {
    fn admit_input(
        &self,
        text: &str,
        commands_enabled: bool,
    ) -> Result<Option<Box<dyn Send>>, String> {
        // Extension commands are handled before model admission. This one command
        // must be able to close the gate it coordinates; literal model input cannot.
        if self.1
            && commands_enabled
            && text.starts_with("/update")
            && text.split_whitespace().next() == Some("/update")
        {
            return Ok(None);
        }
        Ok(Some(Box::new(self.0.enter()?)))
    }
    fn persisted_input(&self, text: &str) -> Result<String, String> {
        pi_session::SessionDataPolicy::persisted_input(
            &crate::bitlocker::credentials::PrivacyPolicy,
            text,
        )
    }
    fn persisted_message(
        &self,
        message: pi_agent::AgentMessage,
    ) -> Result<pi_agent::AgentMessage, String> {
        pi_session::SessionDataPolicy::persisted_message(
            &crate::bitlocker::credentials::PrivacyPolicy,
            message,
        )
    }
    fn project_event(
        &self,
        event: pi_session::AgentSessionEvent,
    ) -> Result<pi_session::AgentSessionEvent, String> {
        pi_session::SessionDataPolicy::project_event(
            &crate::bitlocker::credentials::PrivacyPolicy,
            event,
        )
    }
}

use crate::AgentSessionEvent;
use pi_agent::AgentMessage;

/// Host-owned data projections. The execution/model context stays separate from
/// persisted input copies and subscriber events. Implementations must not call
/// back into the session. Errors stop the affected write or event delivery.
pub trait SessionDataPolicy: Send + Sync {
    /// Acquired before durable input admission and held through run/queue admission.
    /// A host may reject temporarily without poisoning the session. Dropping the
    /// returned lease ends admission; it must not call back into the session.
    fn admit_input(
        &self,
        _text: &str,
        _commands_enabled: bool,
    ) -> Result<Option<Box<dyn Send>>, String> {
        Ok(None)
    }

    fn persisted_input(&self, text: &str) -> Result<String, String> {
        Ok(text.to_owned())
    }

    /// Runs after extension message finalizers and before durable append.
    fn persisted_message(&self, message: AgentMessage) -> Result<AgentMessage, String> {
        Ok(message)
    }

    /// Runs once before either synchronous or asynchronous listeners are called.
    fn project_event(&self, event: AgentSessionEvent) -> Result<AgentSessionEvent, String> {
        Ok(event)
    }
}

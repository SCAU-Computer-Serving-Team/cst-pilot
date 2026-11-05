use std::sync::Arc;

use pi_ai::{Models, faux::register_faux_provider};
use pi_session::{AgentSession, AgentSessionOptions, SessionManager};

#[test]
fn subscription_handles_do_not_keep_session_runtime_alive() {
    for asynchronous in [false, true] {
        let models = Arc::new(Models::default());
        let model = register_faux_provider(&models).model;
        let manager = SessionManager::in_memory("/drop-subscription", None, None).unwrap();
        let session = AgentSession::new(AgentSessionOptions::new(
            "/drop-subscription",
            model,
            models,
            manager,
        ))
        .unwrap();
        let token = session.extension_runner().runtime().cancellation_token();
        let unsubscribe = if asynchronous {
            session.subscribe_async(Arc::new(|_| Box::pin(async {})))
        } else {
            session.subscribe(Arc::new(|_| {}))
        };
        let clone = session.clone();
        drop(session);
        assert!(!token.is_cancelled(), "a session handle is still alive");
        drop(clone);
        assert!(token.is_cancelled(), "unsubscribe handle prevented cleanup");
        unsubscribe();
    }
}

use pi_agent::AgentMessage;
use pi_ai::{
    Models, UserContent,
    faux::{FauxResponseStep, faux_text_message, register_faux_provider},
};
use pi_session::{
    AgentSession, AgentSessionEvent, AgentSessionOptions, PromptOptions, SessionDataPolicy,
    SessionManager,
};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("session-data-policy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn session(f: &Fixture, policy: Option<Arc<dyn SessionDataPolicy>>) -> AgentSession {
    let models = Arc::new(Models::default());
    let faux = register_faux_provider(&models);
    faux.set_responses(vec![FauxResponseStep::Static(faux_text_message("done")); 4]);
    let manager = SessionManager::create(f.0.to_string_lossy(), &f.0, None).unwrap();
    let mut opts = AgentSessionOptions::new(f.0.to_string_lossy(), faux.model, models, manager);
    opts.data_policy = policy;
    AgentSession::new(opts).unwrap()
}
struct Redactor {
    events: AtomicUsize,
}
impl SessionDataPolicy for Redactor {
    fn persisted_input(&self, text: &str) -> Result<String, String> {
        Ok(text.replace("sensitive", "hidden"))
    }
    fn persisted_message(&self, mut message: AgentMessage) -> Result<AgentMessage, String> {
        if let AgentMessage::User(user) = &mut message {
            user.content = UserContent::Text("hidden".into());
        }
        Ok(message)
    }
    fn project_event(&self, mut event: AgentSessionEvent) -> Result<AgentSessionEvent, String> {
        self.events.fetch_add(1, Ordering::SeqCst);
        if let AgentSessionEvent::MessageStart {
            message: AgentMessage::User(user),
        } = &mut event
        {
            user.content = UserContent::Text("projected".into());
        }
        Ok(event)
    }
}
#[tokio::test]
async fn policy_projects_input_before_state_log_and_shares_one_projection_with_both_listener_types()
{
    let f = Fixture::new();
    let policy = Arc::new(Redactor {
        events: AtomicUsize::new(0),
    });
    let s = session(&f, Some(policy.clone()));
    let sync = Arc::new(Mutex::new(Vec::new()));
    let asynchronous = Arc::new(Mutex::new(Vec::new()));
    let events = sync.clone();
    let _unsub = s.subscribe(Arc::new(move |e| events.lock().unwrap().push(e)));
    let events = asynchronous.clone();
    let _unsub_async = s.subscribe_async(Arc::new(move |e| {
        let events = events.clone();
        Box::pin(async move {
            events.lock().unwrap().push(e);
        })
    }));
    let admitted = Arc::new(Mutex::new(None));
    let accepted = admitted.clone();
    s.prompt(
        "sensitive",
        Some(PromptOptions {
            client_message_id: Some("input-1".into()),
            on_admitted: Some(Arc::new(move |result| {
                *accepted.lock().unwrap() = Some(result)
            })),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert!(admitted.lock().unwrap().as_ref().unwrap().is_ok());
    s.with_execution_log(|log| assert_eq!(log.get_input("input-1").unwrap().text, "hidden"));
    let state_path = format!("{}.state.json", s.session_file().unwrap().display());
    assert!(
        !fs::read_to_string(state_path)
            .unwrap()
            .contains("sensitive")
    );
    let jsonl = fs::read_to_string(s.session_file().unwrap()).unwrap();
    assert!(!jsonl.contains("sensitive"));
    assert!(jsonl.contains("hidden"));
    let sync = sync.lock().unwrap().clone();
    let asynchronous = asynchronous.lock().unwrap().clone();
    for event in &asynchronous {
        assert!(sync.contains(event));
    }
    assert!(sync.iter().any(|event| matches!(event, AgentSessionEvent::MessageStart{message:AgentMessage::User(user)} if user.content==UserContent::Text("projected".into()))));
    assert_eq!(policy.events.load(Ordering::SeqCst), sync.len());
    s.shutdown().await.unwrap();
}
struct Failing {
    stage: &'static str,
    panic: bool,
    wrong_role: bool,
}
impl Failing {
    fn fail<T>(&self) -> Result<T, String> {
        if self.panic {
            panic!("private provider body");
        }
        Err("private provider body".into())
    }
}
impl SessionDataPolicy for Failing {
    fn persisted_input(&self, text: &str) -> Result<String, String> {
        if self.stage == "input" {
            self.fail()
        } else {
            Ok(text.into())
        }
    }
    fn persisted_message(&self, message: AgentMessage) -> Result<AgentMessage, String> {
        if self.wrong_role {
            return Ok(AgentMessage::User(pi_ai::UserMessage::new("wrong")));
        }
        if self.stage == "message" {
            self.fail()
        } else {
            Ok(message)
        }
    }
    fn project_event(&self, event: AgentSessionEvent) -> Result<AgentSessionEvent, String> {
        if self.stage == "event" {
            self.fail()
        } else {
            Ok(event)
        }
    }
}
#[tokio::test]
async fn input_projection_error_or_panic_rejects_admission_without_storing_original_or_returning_private_error()
 {
    for panic in [false, true] {
        let f = Fixture::new();
        let s = session(
            &f,
            Some(Arc::new(Failing {
                stage: "input",
                panic,
                wrong_role: false,
            })),
        );
        let accepted = Arc::new(Mutex::new(None));
        let copy = accepted.clone();
        let error = s
            .prompt(
                "sensitive",
                Some(PromptOptions {
                    client_message_id: Some("input".into()),
                    on_admitted: Some(Arc::new(move |r| *copy.lock().unwrap() = Some(r))),
                    ..Default::default()
                }),
            )
            .await
            .unwrap_err();
        assert_eq!(error, "Session input projection failed");
        assert!(accepted.lock().unwrap().as_ref().unwrap().is_err());
        s.with_execution_log(|log| assert!(log.get_input("input").is_none()));
        let _ = s.shutdown().await;
    }
}
#[tokio::test]
async fn message_projection_error_or_panic_never_appends_unprojected_user() {
    for panic in [false, true] {
        let f = Fixture::new();
        let s = session(
            &f,
            Some(Arc::new(Failing {
                stage: "message",
                panic,
                wrong_role: false,
            })),
        );
        let error = s.prompt("sensitive", None).await.unwrap_err();
        assert_eq!(error, "Session message projection failed");
        s.with_session_manager(|sm| {
            assert!(
                !serde_json::to_string(&sm.get_entries())
                    .unwrap()
                    .contains("sensitive")
            )
        });
        let _ = s.shutdown().await;
    }
}
#[tokio::test]
async fn event_projection_error_or_panic_reaches_neither_sync_nor_async_subscribers() {
    for panic in [false, true] {
        let f = Fixture::new();
        let s = session(
            &f,
            Some(Arc::new(Failing {
                stage: "event",
                panic,
                wrong_role: false,
            })),
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let copy = calls.clone();
        let _unsub = s.subscribe(Arc::new(move |_| {
            copy.fetch_add(1, Ordering::SeqCst);
        }));
        let copy = calls.clone();
        let _unsub_async = s.subscribe_async(Arc::new(move |_| {
            copy.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {})
        }));
        assert_eq!(
            s.prompt("sensitive", None).await.unwrap_err(),
            "Session event projection failed"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let _ = s.shutdown().await;
    }
}
#[tokio::test]
async fn message_projection_must_preserve_role() {
    let f = Fixture::new();
    let s = session(
        &f,
        Some(Arc::new(Failing {
            stage: "",
            panic: false,
            wrong_role: true,
        })),
    );
    assert_eq!(
        s.prompt("hello", None).await.unwrap_err(),
        "Session message projection failed"
    );
    let _ = s.shutdown().await;
}
#[tokio::test]
async fn absent_policy_preserves_regular_input_and_client_deduplication() {
    let f = Fixture::new();
    let s = session(&f, None);
    let opts = PromptOptions {
        client_message_id: Some("same".into()),
        ..Default::default()
    };
    s.prompt("sensitive", Some(opts.clone())).await.unwrap();
    s.prompt("different", Some(opts)).await.unwrap();
    s.with_execution_log(|log| assert_eq!(log.get_input("same").unwrap().text, "sensitive"));
    let messages = s.state().messages;
    assert_eq!(messages.iter().filter(|m| m.role() == "user").count(), 1);
    s.shutdown().await.unwrap();
}

struct Admission {
    denied: std::sync::atomic::AtomicBool,
    active: Arc<AtomicUsize>,
    commands: Mutex<Vec<bool>>,
}
struct AdmissionLease(Arc<AtomicUsize>);
impl Drop for AdmissionLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl SessionDataPolicy for Admission {
    fn admit_input(
        &self,
        _: &str,
        commands_enabled: bool,
    ) -> Result<Option<Box<dyn Send>>, String> {
        self.commands.lock().unwrap().push(commands_enabled);
        if self.denied.load(Ordering::SeqCst) {
            return Err("private host admission reason".into());
        }
        self.active.fetch_add(1, Ordering::SeqCst);
        Ok(Some(Box::new(AdmissionLease(self.active.clone()))))
    }
    fn persisted_input(&self, text: &str) -> Result<String, String> {
        assert_eq!(self.active.load(Ordering::SeqCst), 1);
        Ok(text.into())
    }
}
#[tokio::test]
async fn admission_declines_before_durable_input_without_poisoning_and_releases_lease() {
    let f = Fixture::new();
    let policy = Arc::new(Admission {
        denied: true.into(),
        active: Arc::new(AtomicUsize::new(0)),
        commands: Mutex::default(),
    });
    let s = session(&f, Some(policy.clone()));
    let admitted = Arc::new(Mutex::new(None));
    let copy = admitted.clone();
    let options = PromptOptions {
        client_message_id: Some("admission-1".into()),
        on_admitted: Some(Arc::new(move |r| *copy.lock().unwrap() = Some(r))),
        ..Default::default()
    };
    assert_eq!(
        s.prompt("synthetic", Some(options.clone()))
            .await
            .unwrap_err(),
        "Session input admission declined"
    );
    assert!(admitted.lock().unwrap().as_ref().unwrap().is_err());
    assert!(s.fatal_runtime_error().is_none());
    s.with_execution_log(|log| assert!(log.get_input("admission-1").is_none()));
    policy.denied.store(false, Ordering::SeqCst);
    s.prompt("synthetic", Some(options)).await.unwrap();
    assert_eq!(policy.active.load(Ordering::SeqCst), 0);
    s.shutdown().await.unwrap();
}
#[tokio::test]
async fn admission_observes_literal_input_command_flag_and_releases_after_runs() {
    let f = Fixture::new();
    let policy = Arc::new(Admission {
        denied: false.into(),
        active: Arc::new(AtomicUsize::new(0)),
        commands: Mutex::default(),
    });
    let s = session(&f, Some(policy.clone()));
    s.prompt(
        "/ordinary-literal",
        Some(PromptOptions {
            expand_prompt_templates: Some(false),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    s.prompt("another", None).await.unwrap();
    assert_eq!(*policy.commands.lock().unwrap(), vec![false, true]);
    assert_eq!(policy.active.load(Ordering::SeqCst), 0);
    s.shutdown().await.unwrap();
}

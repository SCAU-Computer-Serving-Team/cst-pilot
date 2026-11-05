use std::collections::BTreeMap;
use std::sync::Arc;

use pi_agent::AgentMessage;
use pi_ai::{
    AnyModel, ClassifierContext, ClassifierModel, ClassifierResult, ClassifierStopReason,
    ModelInputModality, ProviderApiConfig, ProviderClassifier, UserMessage,
};
use pi_session::{SessionManager, TreeFilterMode, build_filtered_tree};

#[tokio::test]
async fn virtual_overlay_preserves_classifiers_filters_and_live_refresh() {
    let models = pi_ai::Models::default();
    let physical = pi_ai::faux::register_faux_provider(&models).model;
    let classifier = ClassifierModel {
        model_type: "classifier".into(),
        id: "classifier".into(),
        name: "classifier".into(),
        api: "test-classifier".into(),
        provider: physical.provider.clone(),
        base_url: String::new(),
        input: vec![ModelInputModality::Text],
        input_limits: None,
        cost: Default::default(),
        headers: None,
        context_window: 1000,
        compat: None,
    };
    let mut refreshed = physical.clone();
    refreshed.id = "dynamic".into();
    let fetched = refreshed.clone();
    let base = pi_ai::create_provider(pi_ai::CreateProviderOptions {
        id: physical.provider.clone(),
        name: None,
        base_url: None,
        headers: None,
        auth: pi_ai::ProviderAuth::api_key(pi_ai::ApiKeyAuth::keyless("test", "local")),
        models: vec![
            AnyModel::Chat(physical.clone()),
            AnyModel::Classifier(classifier.clone()),
        ],
        fetch_models: Some(Arc::new(move |_| {
            let model = fetched.clone();
            Box::pin(async move { Ok(vec![AnyModel::Chat(model)]) })
        })),
        filter_models: Some(Arc::new(|_, _| Vec::new())),
        api: Some(ProviderApiConfig::ByApi(pi_ai::default_api_map())),
        classifiers: Some(BTreeMap::from([(
            "test-classifier".into(),
            ProviderClassifier::new(Arc::new(|model, _, _| {
                let model = model.clone();
                Box::pin(async move {
                    ClassifierResult {
                        api: model.api,
                        provider: model.provider,
                        model: model.id,
                        answers: BTreeMap::new(),
                        usage: None,
                        stop_reason: ClassifierStopReason::Stop,
                        error_message: None,
                        timestamp: 0,
                    }
                })
            })),
        )])),
    })
    .unwrap();
    let mut virtual_model = physical.clone();
    virtual_model.id = "route".into();
    virtual_model.api = pi_session::VIRTUAL_MODEL_API.into();
    let overlay =
        pi_session::with_virtual_models(base.id(), Some(base.clone()), vec![virtual_model]);
    assert!(overlay.has_dynamic_refresh());
    assert_eq!(overlay.get_classifiers(), vec![classifier.clone()]);
    assert_eq!(
        overlay
            .filter_chat_models(&overlay.get_models(), None)
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        vec!["route"]
    );
    // Classification remains functional, not just listed in the catalog.
    let context = ClassifierContext {
        state: Default::default(),
        images: None,
        questions: BTreeMap::new(),
    };
    assert_eq!(
        overlay
            .classify(&classifier, &context, None)
            .await
            .stop_reason,
        ClassifierStopReason::Stop
    );
    models.set_provider(overlay.clone());
    let result = models.refresh(None).await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(overlay.get_models().iter().any(|m| m.id == refreshed.id));
    assert!(base.get_models().iter().any(|m| m.id == refreshed.id));
    assert!(overlay.get_models().iter().any(|m| m.id == "route"));
}

#[test]
fn all_tree_retains_usage_and_tool_only_assistants_and_labeled_filter_retains_them() {
    let mut sm = SessionManager::in_memory("/proj", None, None).unwrap();
    sm.append_message(AgentMessage::User(UserMessage::new("question")))
        .unwrap();
    let mut assistant = pi_ai::faux::faux_text_message("");
    assistant.content.clear();
    let assistant_id = sm
        .append_message(AgentMessage::Assistant(assistant))
        .unwrap();
    let usage = sm
        .append_usage("summary", "faux", "faux", Default::default(), None)
        .unwrap();
    sm.append_label_change(&assistant_id, Some("tool turn".into()))
        .unwrap();
    let roots = sm.get_tree();
    let all = build_filtered_tree(&roots, sm.get_leaf_id(), TreeFilterMode::All, None, None);
    assert_eq!(all.len(), sm.get_entries().len());
    assert!(all.iter().any(|row| row.entry.id() == usage.id));
    let default = build_filtered_tree(
        &roots,
        sm.get_leaf_id(),
        TreeFilterMode::Default,
        None,
        None,
    );
    assert!(default.iter().any(|row| row.entry.id() == assistant_id));
    assert!(!default.iter().any(|row| row.entry.id() == usage.id));
    let labeled = build_filtered_tree(
        &roots,
        sm.get_leaf_id(),
        TreeFilterMode::LabeledOnly,
        None,
        None,
    );
    assert_eq!(labeled.len(), 1);
    assert_eq!(labeled[0].entry.id(), assistant_id);
}

#[test]
fn structured_prompt_renders_stable_prefix_before_volatile_sections() {
    let prompt = pi_session::build_system_prompt(&pi_session::BuildSystemPromptOptions {
        cwd: "E:\\project".into(),
        append_system_prompt: Some("extra".into()),
        sections: Some(BTreeMap::from([("custom".into(), "tail".into())])),
        ..Default::default()
    })
    .unwrap();
    assert!(prompt.starts_with("You are"));
    let offsets: Vec<_> = [
        "<tools>",
        "<rules>",
        "<docs>",
        "<addendum>",
        "<cwd>",
        "<custom>",
    ]
    .iter()
    .map(|section| prompt.find(section).unwrap())
    .collect();
    assert!(offsets.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn atomic_input_correlation_includes_queue_identity() {
    let mut sm = SessionManager::in_memory("/proj", None, None).unwrap();
    let id = sm
        .append_queued_user_message(
            AgentMessage::User(UserMessage::new("question")),
            "client",
            Some("queue"),
        )
        .unwrap();
    let entry = sm
        .get_entries()
        .into_iter()
        .find_map(|entry| match entry {
            pi_session::SessionEntry::Custom(c) => c.data,
            _ => None,
        })
        .unwrap();
    assert_eq!(entry["userEntryId"], id);
    assert_eq!(entry["queueItemId"], "queue");
}

#[test]
fn invalid_boundary_anchors_and_content_blocks_are_rejected_atomically() {
    use pi_session::{ContextEditReplacement, SessionBoundaryDraft};
    let mut sm = SessionManager::in_memory("/proj", None, None).unwrap();
    let user = sm
        .append_message(AgentMessage::User(UserMessage::new("root")))
        .unwrap();
    let old_branch = sm
        .append_message(AgentMessage::User(UserMessage::new("other branch")))
        .unwrap();
    sm.branch(&user).unwrap();
    let before = sm.get_entries();
    let drafts = [
        SessionBoundaryDraft::Custom {
            custom_type: "must not commit".into(),
            data: None,
        },
        SessionBoundaryDraft::Compaction {
            summary: "invalid anchor".into(),
            first_kept_entry_id: Some(old_branch),
            tokens_before: None,
            details: None,
            usage: None,
            from_hook: None,
        },
    ];
    assert!(sm.commit_boundary_drafts(&drafts).is_err());
    assert_eq!(sm.get_entries(), before);
    assert!(
        sm.append_context_edit(
            &user,
            Some(ContextEditReplacement::from_value(
                serde_json::json!([{"type":"invalid"}])
            ))
        )
        .is_err()
    );
    assert_eq!(sm.get_entries(), before);
}

#[test]
fn malformed_timestamps_never_panic_and_timezone_offsets_are_applied() {
    use pi_session::parse_iso_timestamp_ms;
    for value in [
        "2026-00-00T00:00:00Z",
        "2026-11-00T00:00:00Z",
        "2026-02-29T00:00:00Z",
        "2026-11-04T99:00:00Z",
        "2026-11-04T00:00:00.bad",
    ] {
        assert_eq!(parse_iso_timestamp_ms(value), None, "{value}");
    }
    assert_eq!(
        parse_iso_timestamp_ms("2026-11-04T08:00:00.123+08:00"),
        parse_iso_timestamp_ms("2026-11-04T00:00:00.123Z")
    );
}

#[tokio::test]
async fn captured_ui_handles_cannot_modify_ui_after_invalidation() {
    use pi_session::{
        ChannelUiContext, ExtensionMode, ExtensionRunner, ExtensionUiContext,
        WorkingIndicatorOptions,
    };
    let runner = ExtensionRunner::from_extensions("/proj", Vec::new()).unwrap();
    let (ui, mut requests) = ChannelUiContext::pair();
    runner.set_ui_context(Some(Arc::new(ui.clone())), ExtensionMode::Interactive);
    let handle = runner.ui_context();
    handle.set_working_indicator(Some(WorkingIndicatorOptions {
        frames: Some(vec!["a".into()]),
        interval_ms: Some(100),
    }));
    assert_eq!(
        ui.snapshot().working_indicator.unwrap().interval_ms,
        Some(100)
    );
    runner.invalidate(None);
    handle.set_title("stale");
    handle.set_editor_text("stale");
    assert!(
        handle
            .input("stale", None, None, Default::default())
            .await
            .is_none()
    );
    assert!(requests.try_recv().is_err());
    assert!(ui.snapshot().title.is_none());
    assert!(ui.get_editor_text().is_empty());
}

#[test]
fn custom_session_backend_is_used_for_initialization_and_appends() {
    use pi_session::{
        FileEntry, InMemorySessionStore, SessionEntry, SessionStore, SessionStoreError,
    };
    struct Store(Arc<std::sync::Mutex<InMemorySessionStore>>);
    impl SessionStore for Store {
        fn location(&self) -> Option<&std::path::Path> {
            None
        }
        fn is_persisted(&self) -> bool {
            false
        }
        fn load_entries(&mut self) -> Result<Vec<FileEntry>, SessionStoreError> {
            self.0.lock().unwrap().load_entries()
        }
        fn rewrite_all(
            &mut self,
            entries: &[FileEntry],
            exclusive: bool,
        ) -> Result<(), SessionStoreError> {
            self.0.lock().unwrap().rewrite_all(entries, exclusive)
        }
        fn append_entry(&mut self, entry: &SessionEntry) -> Result<(), SessionStoreError> {
            self.0.lock().unwrap().append_entry(entry)
        }
    }
    let storage = Arc::new(std::sync::Mutex::new(InMemorySessionStore::new()));
    let mut sm =
        SessionManager::from_store("/proj", Box::new(Store(storage.clone())), None).unwrap();
    sm.append_message(AgentMessage::User(UserMessage::new("question")))
        .unwrap();
    assert_eq!(storage.lock().unwrap().entries().len(), 2);
    drop(sm);
    let reopened = SessionManager::from_store("/proj", Box::new(Store(storage)), None).unwrap();
    assert_eq!(reopened.get_entries().len(), 1);
}

#[tokio::test]
async fn execution_log_factory_runs_for_initial_and_replacement_sessions() {
    use pi_session::{AgentSession, AgentSessionOptions, ExecutionLogController};
    let models = Arc::new(pi_ai::Models::default());
    let model = pi_ai::faux::register_faux_provider(&models).model;
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = seen.clone();
    let mut options = AgentSessionOptions::new(
        "/proj",
        model,
        models,
        SessionManager::in_memory("/proj", None, None).unwrap(),
    );
    options.execution_log_factory = Some(Arc::new(move |sm| {
        captured
            .lock()
            .unwrap()
            .push(sm.get_session_id().to_owned());
        Ok(ExecutionLogController::in_memory())
    }));
    let session = AgentSession::new(options).unwrap();
    let initial_id = session.session_id();
    assert!(session.new_session(None).await.unwrap());
    assert_eq!(
        *seen.lock().unwrap(),
        vec![initial_id, session.session_id()]
    );
}

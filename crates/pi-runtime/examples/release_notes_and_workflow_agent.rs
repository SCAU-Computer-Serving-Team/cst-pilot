//! Run with `cargo run -p pi-runtime --example release_notes_and_workflow_agent --locked`.
//! Demonstrates a reusable SDK factory, skills, persistent forks and UI replies without network.
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use pi_agent::AgentToolResult;
use pi_ai::faux::{
    create_faux_provider, faux_assistant_message, faux_text_message, faux_tool_call,
};
use pi_ai::{ModelThinkingLevel, StopReason};
use pi_runtime::*;
use pi_session::{
    ChannelUiContext, ExtensionFactory, ExtensionMode, InlineExtension, SessionManager,
    ToolDefinition, UiPromptRequest,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("pi-release-workflow-{}", pi_ai::uuid_v7()));
    let skill = root.join("skills/release-notes");
    std::fs::create_dir_all(&skill)?;
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: release-notes\ndescription: Draft release notes from reviewed changes.\n---\nGroup changes into Features and Fixes. Ask approval before finalizing.\n",
    )?;
    let faux = create_faux_provider();
    faux.set_responses([
        faux_text_message(
            "## Features\n- Bounded retries.\n\n## Fixes\n- Preserve concurrent settings updates.",
        )
        .into(),
        faux_assistant_message(
            vec![faux_tool_call(
                "approve_release",
                Default::default(),
                Some("approval".into()),
            )],
            Some(StopReason::ToolUse),
        )
        .into(),
        faux_text_message("Release v0.1.0 approved. Publish the reviewed notes after CI passes.")
            .into(),
    ]);
    let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        include_builtins: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await;
    let provider = faux.provider.clone();
    let extension: ExtensionFactory = Arc::new(move || {
        let provider = provider.clone();
        Ok(Box::new(InlineExtension::new(
            "<release-workflow>",
            move |api| {
                // Re-register the mock provider for each isolated session catalog.
                api.register_provider(provider.clone());
                api.register_tool(ToolDefinition::new(
                    "approve_release",
                    "Approve release",
                    "Ask for release approval and version",
                    json!({"type":"object","properties":{}}),
                    Arc::new(|_, _, cancel, _, ctx| {
                        Box::pin(async move {
                            let cancel = cancel.unwrap_or_default();
                            if !ctx
                                .ui()
                                .confirm(
                                    "Approve release",
                                    "Finalize the reviewed notes?",
                                    None,
                                    cancel.clone(),
                                )
                                .await
                            {
                                return Err("Release approval declined".into());
                            }
                            let version = ctx
                                .ui()
                                .input("Release version", Some("v0.1.0"), None, cancel)
                                .await
                                .ok_or("Release version missing")?;
                            Ok(AgentToolResult::text(format!("Approved {version}")))
                        })
                    }),
                ));
                Ok(())
            },
        )))
    });
    let services = create_agent_session_services(CreateAgentSessionServicesOptions {
        cwd: root.clone(),
        agent_dir: Some(root.join("agent")),
        model_runtime: Some(model_runtime),
        resource_loader_options: Some(DefaultResourceLoaderOptions {
            extension_factories: vec![extension],
            additional_skill_paths: vec![skill],
            no_context_files: true,
            no_prompt_templates: true,
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;
    assert!(services.diagnostics.iter().all(|d| d.kind != "error"));
    assert_eq!(services.resource_loader.get_skills().skills.len(), 1);
    let (ui, mut requests) = ChannelUiContext::pair();
    let replies = Arc::new(AtomicUsize::new(0));
    let (reply_count, ui_cancel) = (replies.clone(), CancellationToken::new());
    let stop_ui = ui_cancel.clone();
    let responder = tokio::spawn(async move {
        loop {
            let request =
                tokio::select! { _ = stop_ui.cancelled() => break, r = requests.recv() => r };
            match request {
                Some(UiPromptRequest::Confirm { respond, .. }) => {
                    let _ = respond.send(true);
                }
                Some(UiPromptRequest::Input { respond, .. }) => {
                    let _ = respond.send(Some("v0.1.0".into()));
                }
                None => break,
                _ => continue,
            }
            reply_count.fetch_add(1, Ordering::SeqCst);
        }
    });
    let model = faux.model.clone();
    let factory: CreateAgentSessionRuntimeFactory = Arc::new(move |options| {
        let (services, model, ui) = (services.clone(), model.clone(), ui.clone());
        Box::pin(async move {
            let created =
                create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
                    services: services.clone(),
                    session_manager: options.session_manager,
                    execution_log: None,
                    execution_log_factory: None,
                    session_start_event: options.session_start_event,
                    model: Some(model),
                    thinking_level: Some(ModelThinkingLevel::Off),
                    scoped_models: vec![],
                    initial_model_policy: InitialModelPolicy::GenericSdk,
                    no_tools: Some(NoToolsMode::Builtin),
                    tools: None,
                    exclude_tools: None,
                    custom_tools: vec![],
                    tools_options: None,
                    ui_context: Some(Arc::new(ui)),
                    mode: Some(ExtensionMode::Interactive),
                    tool_execution: None,
                    clock: None,
                })
                .await?;
            Ok(CreateAgentSessionRuntimeResult {
                session: created.session,
                extensions_result: created.extensions_result,
                diagnostics: services.diagnostics.clone(),
                model_fallback_message: created.model_fallback_message,
                services,
            })
        })
    });
    let host = create_agent_session_runtime(
        factory,
        CreateRuntimeOptions {
            cwd: root.clone(),
            agent_dir: root.join("agent"),
            session_manager: SessionManager::create(
                root.to_string_lossy(),
                root.join("sessions"),
                None,
            )?,
            session_start_event: Some(("startup".into(), None)),
        },
    )
    .await?;
    let result: Result<(), String> = async {
        let original = host.session();
        original
            .prompt(
                "/skill:release-notes changes: bounded retries; settings concurrency",
                None,
            )
            .await?;
        let leaf = original
            .with_session_manager(|sm| sm.get_leaf_id().map(str::to_owned))
            .ok_or("No release draft leaf")?;
        let draft_file = original.session_file().ok_or("No draft session file")?;
        let outcome = host
            .fork(
                &leaf,
                Some(ForkSessionRuntimeOptions {
                    position: Some(ForkPosition::At),
                    ..Default::default()
                }),
            )
            .await?;
        assert!(!outcome.cancelled);
        assert!(original.is_closed());
        assert_ne!(host.session().session_file(), Some(draft_file.clone()));
        assert!(SessionManager::open(draft_file, None, None).is_ok());
        host.session()
            .prompt(
                "Explore a release checklist and ask approval using approve_release.",
                None,
            )
            .await?;
        assert_eq!(replies.load(Ordering::SeqCst), 2);
        assert_eq!(faux.call_count(), 3);
        Ok(())
    }
    .await;
    let closed = host.dispose().await;
    ui_cancel.cancel();
    responder.await?;
    result?;
    closed?;
    println!(
        "Release notes and workflow example passed: {}",
        root.display()
    );
    Ok(())
}

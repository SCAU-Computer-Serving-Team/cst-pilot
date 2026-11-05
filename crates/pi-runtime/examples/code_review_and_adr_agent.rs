//! Run with `cargo run -p pi-runtime --example code_review_and_adr_agent --locked`.
//! Uses an isolated workspace and an in-process FauxProvider; no credentials or network.
use pi_ai::faux::{
    create_faux_provider, faux_assistant_message, faux_text_message, faux_tool_call,
};
use pi_ai::{ModelThinkingLevel, StopReason};
use pi_runtime::{
    CreateAgentSessionOptions, CreateModelRuntimeOptions, ModelRuntime, create_agent_session,
};
use pi_session::SessionManager;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("pi-review-adr-{}", pi_ai::uuid_v7()));
    std::fs::create_dir_all(root.join("src"))?;
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn retry_limit() -> usize { 3 }\n",
    )?;
    let faux = create_faux_provider();
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        include_builtins: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await;
    runtime.register_provider(faux.provider.clone());
    let adr = "# ADR 001: Bound request retries\n\n## Decision\nUse three attempts.\n\n## Consequences\nRequests have a fixed retry budget.\n";
    faux.set_responses([
        faux_assistant_message(
            vec![faux_tool_call(
                "read",
                json!({"path":"src/lib.rs"}).as_object().unwrap().clone(),
                Some("review-source".into()),
            )],
            Some(StopReason::ToolUse),
        )
        .into(),
        faux_assistant_message(
            vec![faux_tool_call(
                "write",
                json!({"path":"doc/adr/001-retry-budget.md","content":adr})
                    .as_object()
                    .unwrap()
                    .clone(),
                Some("record-decision".into()),
            )],
            Some(StopReason::ToolUse),
        )
        .into(),
        faux_text_message("Review: retry_limit is bounded at three. ADR 001 records the decision.")
            .into(),
    ]);
    let created = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(root.clone()),
        agent_dir: Some(root.join("agent")),
        model_runtime: Some(runtime),
        model: Some(faux.model.clone()),
        thinking_level: Some(ModelThinkingLevel::Off),
        tools: Some(vec!["read".into(), "write".into()]),
        session_manager: Some(SessionManager::create(
            root.to_string_lossy(),
            root.join("sessions"),
            None,
        )?),
        ..Default::default()
    })
    .await?;
    // Always await shutdown, including when the request fails.
    let result = created
        .session
        .prompt(
            "Review src/lib.rs and write an ADR for the retry budget.",
            None,
        )
        .await;
    let closed = created.session.shutdown().await;
    result?;
    closed?;
    assert_eq!(
        std::fs::read_to_string(root.join("doc/adr/001-retry-budget.md"))?,
        adr
    );
    assert_eq!(faux.call_count(), 3);
    assert_eq!(faux.pending_response_count(), 0);
    let path = created
        .session
        .session_file()
        .expect("persistent session path");
    assert!(SessionManager::open(path, None, None).is_ok());
    println!("Code review and ADR example passed: {}", root.display());
    Ok(())
}

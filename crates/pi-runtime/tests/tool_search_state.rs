use pi_ai::StopReason;
use pi_ai::faux::{
    FauxResponseStep, create_faux_provider, faux_assistant_message, faux_text_message,
    faux_tool_call,
};
use pi_runtime::{
    CreateAgentSessionOptions, CreateModelRuntimeOptions, ModelRuntime, create_agent_session,
};
use pi_session::{
    ForkSessionOptions, InlineExtension, NavigateTreeOptions, SessionManager, ToolDefinition,
    ToolExposure,
};
use serde_json::json;
use std::sync::Arc;

fn deferred() -> ToolDefinition {
    ToolDefinition::new(
        "mcp__docs__search",
        "search",
        "Search documentation",
        json!({"type":"object"}),
        Arc::new(|_, _, _, _, _| Box::pin(async { Ok(pi_agent::AgentToolResult::text("found")) })),
    )
    .with_exposure(ToolExposure::Deferred)
}

#[tokio::test]
async fn searched_tool_selection_survives_tree_fork_resume_and_delayed_inventory() {
    let root = std::env::temp_dir().join(format!("pi-tool-state-{}", pi_ai::uuid_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let faux = create_faux_provider();
    faux.set_responses([
        FauxResponseStep::Static(faux_text_message("before searching")),
        FauxResponseStep::Static(faux_assistant_message(
            vec![faux_tool_call(
                "tool_search",
                json!({"query":"mcp__docs__search"})
                    .as_object()
                    .unwrap()
                    .clone(),
                Some("search1".to_owned()),
            )],
            Some(StopReason::ToolUse),
        )),
        FauxResponseStep::Dynamic(Arc::new(|context, _, _| {
            assert!(
                pi_ai::get_current_tools(context.messages())
                    .iter()
                    .any(|tool| tool.name == "mcp__docs__search")
            );
            faux_text_message("tool available on next model call")
        })),
    ]);
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        include_builtins: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await;
    runtime.register_provider(faux.provider.clone());
    let inventory: pi_session::ExtensionFactory =
        Arc::new(|| Ok(Box::new(InlineExtension::new("test:inventory", |_| Ok(())))));
    let make_options = |manager| CreateAgentSessionOptions {
        cwd: Some(root.clone()),
        agent_dir: Some(root.join("agent")),
        model: Some(faux.model.clone()),
        model_runtime: Some(runtime.clone()),
        extension_factories: vec![inventory.clone()],
        session_manager: Some(manager),
        ..Default::default()
    };
    let session = create_agent_session(make_options(
        SessionManager::create(root.to_string_lossy(), root.join("sessions"), None).unwrap(),
    ))
    .await
    .unwrap()
    .session;
    let api = session.extension_runner().runtime();
    api.replace_extension_tools("test:inventory", vec![deferred()])
        .unwrap();
    let mut active = api.get_active_tools().unwrap();
    active.push("tool_search".to_owned());
    api.set_active_tools(active).unwrap();
    session.prompt("before", None).await.unwrap();
    let before = session.with_session_manager(|sm| sm.get_leaf_id().unwrap().to_owned());
    session
        .prompt("find documentation tool", None)
        .await
        .unwrap();
    let selected = session.with_session_manager(|sm| sm.get_leaf_id().unwrap().to_owned());
    assert!(
        session
            .get_active_tool_names()
            .contains(&"mcp__docs__search".to_owned())
    );
    let nav_options = || NavigateTreeOptions {
        summarize: false,
        custom_instructions: None,
        replace_instructions: false,
        label: None,
    };
    session.navigate_tree(&before, nav_options()).await.unwrap();
    assert!(
        !session
            .get_active_tool_names()
            .contains(&"mcp__docs__search".to_owned())
    );
    session
        .navigate_tree(&selected, nav_options())
        .await
        .unwrap();
    assert!(
        session
            .get_active_tool_names()
            .contains(&"mcp__docs__search".to_owned())
    );
    assert!(
        session
            .fork(
                &selected,
                ForkSessionOptions {
                    position: Some("at".to_owned())
                }
            )
            .await
            .unwrap()
    );
    assert!(
        session
            .get_active_tool_names()
            .contains(&"mcp__docs__search".to_owned())
    );
    let file = session.session_file().unwrap();
    session.shutdown().await.unwrap();
    drop(api);
    drop(session);
    let manager = SessionManager::open_existing(&file, None, None).unwrap();
    let resumed = create_agent_session(make_options(manager))
        .await
        .unwrap()
        .session;
    let api = resumed.extension_runner().runtime();
    assert!(
        !resumed
            .get_all_tools()
            .iter()
            .any(|tool| tool.name == "mcp__docs__search")
    );
    assert!(
        api.get_requested_tools()
            .unwrap()
            .contains(&"mcp__docs__search".to_owned())
    );
    // A model boundary while the server is absent may remove its declaration, not the selection intent.
    faux.set_responses([FauxResponseStep::Static(faux_text_message(
        "server is reconnecting",
    ))]);
    resumed
        .prompt("continue while reconnecting", None)
        .await
        .unwrap();
    let absent_file = resumed.session_file().unwrap();
    resumed.shutdown().await.unwrap();
    drop(api);
    drop(resumed);
    let resumed = create_agent_session(make_options(
        SessionManager::open_existing(&absent_file, None, None).unwrap(),
    ))
    .await
    .unwrap()
    .session;
    let api = resumed.extension_runner().runtime();
    assert!(
        api.get_requested_tools()
            .unwrap()
            .contains(&"mcp__docs__search".to_owned())
    );
    // A helper activation during asynchronous startup must retain the selected unavailable tool.
    let mut desired = api.get_requested_tools().unwrap();
    desired.push("codemode".to_owned());
    api.set_active_tools(desired).unwrap();
    api.replace_extension_tools("test:inventory", vec![deferred()])
        .unwrap();
    assert!(
        resumed
            .get_active_tool_names()
            .contains(&"mcp__docs__search".to_owned())
    );
    // An intentionally empty loadout stays empty across registry refresh.
    api.set_active_tools(Vec::new()).unwrap();
    api.refresh_tools().unwrap();
    assert!(api.get_active_tools().unwrap().is_empty());
    resumed.shutdown().await.unwrap();
    drop(api);
    drop(resumed);
    std::fs::remove_dir_all(root).unwrap();
}

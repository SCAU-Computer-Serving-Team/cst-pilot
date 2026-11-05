use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_agent::{
    Agent, AgentEvent, AgentInitialState, AgentOptions, AgentTool, AgentToolResult,
    default_agent_identity, models_stream_fn,
};
use pi_ai::faux::{
    faux_assistant_message, faux_text_message, faux_thinking, faux_tool_call,
    register_faux_provider,
};
use pi_ai::{
    ClientIdentity, JsonObject, ModelThinkingLevel, StopReason, create_models,
    merge_client_identity, pi_attribution,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize, JsonSchema)]
struct CalcParams {
    a: i64,
    b: i64,
}

#[tokio::test]
async fn faux_provider_end_to_end_multi_turn_tools_thinking_and_callbacks() {
    let models = Arc::new(create_models(None));
    let faux = register_faux_provider(&models);

    let mut args = JsonObject::new();
    args.insert("a".to_owned(), json!("15"));
    args.insert("b".to_owned(), json!(27));

    faux.set_responses([
        faux_assistant_message(
            vec![
                faux_thinking("Need to add 15 and 27 using the add tool."),
                faux_tool_call("add", args, Some("call_add_1".to_owned())),
            ],
            Some(StopReason::ToolUse),
        )
        .into(),
        faux_text_message("The sum of 15 and 27 is 42.").into(),
    ]);

    let add_tool = AgentTool::from_typed(
        "add",
        "Add",
        "Adds two integers",
        |_id, params: CalcParams, _cancel, _on_update| async move {
            Ok(AgentToolResult::text(format!("{}", params.a + params.b)))
        },
    );

    let payload_calls = Arc::new(AtomicUsize::new(0));
    let response_calls = Arc::new(AtomicUsize::new(0));
    let pc = payload_calls.clone();
    let rc = response_calls.clone();

    let mut opts = AgentOptions::new(models_stream_fn(models));
    opts.initial_state = Some(AgentInitialState {
        system_prompt: Some("You are a calculator assistant.".to_owned()),
        model: Some(faux.model.clone()),
        thinking_level: Some(ModelThinkingLevel::High),
        tools: Some(vec![add_tool]),
        messages: None,
    });
    opts.on_payload = Some(Arc::new(move |payload, _model| {
        pc.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Some(payload) })
    }));
    opts.on_response = Some(Arc::new(move |resp, _model| {
        assert_eq!(resp.status, 200);
        rc.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    }));

    let agent = Agent::new(opts);
    let event_types = Arc::new(Mutex::new(Vec::new()));
    let et_clone = event_types.clone();
    let _unsub = agent.subscribe(Arc::new(move |ev, _cancel| {
        let et_clone = et_clone.clone();
        Box::pin(async move {
            let tag = match ev {
                AgentEvent::AgentStart => "agent_start",
                AgentEvent::AgentEnd { .. } => "agent_end",
                AgentEvent::TurnStart => "turn_start",
                AgentEvent::TurnEnd { .. } => "turn_end",
                AgentEvent::MessageStart { .. } => "message_start",
                AgentEvent::MessageUpdate { .. } => "message_update",
                AgentEvent::MessageEnd { .. } => "message_end",
                AgentEvent::ToolExecutionStart { .. } => "tool_execution_start",
                AgentEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
                AgentEvent::ToolExecutionEnd { .. } => "tool_execution_end",
            };
            et_clone.lock().unwrap().push(tag);
        })
    }));

    agent.prompt("What is 15 + 27?").await.unwrap();

    assert_eq!(faux.call_count(), 2);
    assert_eq!(payload_calls.load(Ordering::SeqCst), 2);
    assert_eq!(response_calls.load(Ordering::SeqCst), 2);

    let state = agent.state();
    assert!(!state.is_streaming);
    assert!(state.pending_tool_calls.is_empty());
    // system + user + assistant(thinking+toolCall) + toolResult + assistant(final) = 5 messages
    assert_eq!(state.messages.len(), 5);
    assert_eq!(
        state.messages[2].as_assistant().unwrap().thinking(),
        "Need to add 15 and 27 using the add tool."
    );
    assert_eq!(
        state.messages[4].as_assistant().unwrap().text(),
        "The sum of 15 and 27 is 42."
    );

    let tags = event_types.lock().unwrap();
    assert_eq!(tags.first(), Some(&"agent_start"));
    assert_eq!(tags.last(), Some(&"agent_end"));
    assert!(tags.contains(&"tool_execution_start"));
    assert!(tags.contains(&"tool_execution_end"));
}

#[test]
fn client_identity_merges_pi_attribution_and_respects_explicit_null_override() {
    let default_id = default_agent_identity();
    assert_eq!(default_id.attribution, Some(Some(pi_attribution())));

    // Merging with an identity that only sets version preserves PI_ATTRIBUTION
    let custom_ver = ClientIdentity {
        version: Some("2.0.0".to_owned()),
        ..Default::default()
    };
    let merged = merge_client_identity([Some(&default_id), Some(&custom_ver)]).unwrap();
    assert_eq!(merged.version.as_deref(), Some("2.0.0"));
    assert_eq!(merged.attribution, Some(Some(pi_attribution())));

    // Deserializing {"attribution": null} produces Some(None), which turns off attribution when merged
    let disable_attr: ClientIdentity =
        serde_json::from_value(json!({ "attribution": null })).unwrap();
    assert_eq!(disable_attr.attribution, Some(None));
    let merged_off = merge_client_identity([Some(&default_id), Some(&disable_attr)]).unwrap();
    assert_eq!(merged_off.attribution, Some(None));
}

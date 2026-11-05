use std::collections::BTreeMap;
use std::sync::Arc;

use pi_agent::AgentToolResult;
use pi_ai::faux::{
    FauxResponseStep, faux_assistant_message, faux_text, faux_tool_call, register_faux_provider,
};
use pi_ai::{JsonObject, Models, StopReason, Usage, UsageCost};
use pi_session::{
    AgentSession, AgentSessionOptions, InlineExtension, NestedToolCallOptions, SessionManager,
    ToolDefinition, ToolExposure, ToolLoadoutChanges,
};
use serde_json::json;

#[tokio::main]
async fn main() {
    let models = Arc::new(Models::default());
    let reg = register_faux_provider(&models);
    let mut call_args = JsonObject::new();
    call_args.insert("x".to_owned(), json!(21));
    reg.set_responses(vec![
        FauxResponseStep::Static(faux_assistant_message(
            vec![faux_tool_call(
                "orchestrate",
                call_args,
                Some("call_orch".to_owned()),
            )],
            Some(StopReason::ToolUse),
        )),
        FauxResponseStep::Static(faux_assistant_message(
            vec![faux_text("Orchestration finished: 42")],
            Some(StopReason::Stop),
        )),
    ]);

    let ext = InlineExtension::new("<example:nested-tools>", |api| {
        let mut double_tool = ToolDefinition::new(
            "math_double",
            "Math Double",
            "Doubles an integer",
            json!({
                "type": "object",
                "properties": { "n": { "type": "integer" } },
                "required": ["n"]
            }),
            Arc::new(|_id, args, _cancel, _upd, _ctx| {
                Box::pin(async move {
                    let n = args["n"].as_i64().unwrap_or(0);
                    let mut res = AgentToolResult::text(format!("{}", n * 2));
                    res.usage = Some(Usage {
                        input: 10,
                        output: 5,
                        cache_read: 0,
                        cache_write: 0,
                        cache_write_1h: None,
                        reasoning: None,
                        total_tokens: 15,
                        cost: UsageCost {
                            input: 0.001,
                            output: 0.001,
                            cache_read: 0.0,
                            cache_write: 0.0,
                            total: 0.002,
                            total_from_provider: Some(true),
                        },
                    });
                    Ok(res)
                })
            }),
        );
        double_tool.exposure = ToolExposure::Codemode;
        api.register_tool(double_tool);

        let mut audit_tool = ToolDefinition::new(
            "audit_log",
            "Audit Log",
            "Records an audit entry",
            json!({
                "type": "object",
                "properties": { "msg": { "type": "string" } },
                "required": ["msg"]
            }),
            Arc::new(|_id, args, _cancel, _upd, _ctx| {
                Box::pin(async move {
                    let msg = args["msg"].as_str().unwrap_or_default();
                    Ok(AgentToolResult::text(format!("audited:{msg}")))
                })
            }),
        );
        audit_tool.exposure = ToolExposure::Deferred;
        api.register_tool(audit_tool);

        let mut orchestrate = ToolDefinition::new(
            "orchestrate",
            "Orchestrate",
            "Base description",
            json!({
                "type": "object",
                "properties": { "x": { "type": "integer" } },
                "required": ["x"]
            }),
            Arc::new(|_id, args, _cancel, _upd, ctx| {
                Box::pin(async move {
                    let x = args["x"].as_i64().unwrap_or(0);
                    let step1 = ctx
                        .execute_tool(
                            "math_double",
                            json!({ "n": x }),
                            NestedToolCallOptions::default(),
                        )
                        .await;
                    let doubled = step1.result.first_text().unwrap_or("0").to_owned();
                    let _step2 = ctx
                        .execute_tool(
                            "audit_log",
                            json!({ "msg": format!("doubled={doubled}") }),
                            NestedToolCallOptions::default(),
                        )
                        .await;
                    Ok(AgentToolResult::text(format!("result={doubled}")))
                })
            }),
        );
        orchestrate.prepare_loadout = Some(Arc::new(|loadout| {
            let codemode_names: Vec<String> = loadout
                .all_tools
                .iter()
                .filter(|t| t.exposure == ToolExposure::Codemode)
                .map(|t| t.name.clone())
                .collect();
            let mut descriptions = BTreeMap::new();
            descriptions.insert(
                "orchestrate".to_owned(),
                format!(
                    "Orchestrator with codemode tools: {}",
                    codemode_names.join(", ")
                ),
            );
            Some(ToolLoadoutChanges {
                descriptions,
                hidden_declarations: vec!["hidden_helper".to_owned()],
            })
        }));
        api.register_tool(orchestrate);

        let helper = ToolDefinition::new(
            "hidden_helper",
            "Hidden Helper",
            "Should be hidden from model declarations by prepare_loadout",
            json!({"type": "object"}),
            Arc::new(|_, _, _, _, _| Box::pin(async { Ok(AgentToolResult::text("ok")) })),
        );
        api.register_tool(helper);
        Ok(())
    });

    let sm = SessionManager::in_memory(".", None, None).unwrap();
    let mut opts = AgentSessionOptions::new(".", reg.model.clone(), models, sm);
    opts.extensions = vec![Box::new(ext)];

    let session = AgentSession::new(opts).unwrap();
    session.prompt("compute 21 * 2", None).await.unwrap();

    let state = session.state();
    let tr = state
        .messages
        .iter()
        .find_map(|m| m.as_tool_result())
        .expect("tool result present");
    assert_eq!(tr.tool_call_id, "call_orch");
    let nested = tr.nested_calls.as_ref().expect("nested_calls merged");
    assert!(nested.complete);
    assert_eq!(nested.calls.len(), 2);
    assert_eq!(nested.calls[0].id, "call_orch/1");
    assert_eq!(nested.calls[0].name, "math_double");
    assert_eq!(nested.calls[1].id, "call_orch/2");
    assert_eq!(nested.calls[1].name, "audit_log");
    assert_eq!(tr.usage.as_ref().unwrap().total_tokens, 15);
    println!("example custom_tools_and_nested_calls passed");
}

use std::sync::Arc;

use pi_agent::AgentToolResult;
use pi_ai::faux::{
    FauxResponseStep, create_faux_provider, faux_assistant_message, faux_text, faux_tool_call,
};
use pi_ai::{JsonObject, ModelThinkingLevel, Models, StopReason};
use pi_session::{
    AgentSession, AgentSessionOptions, Extension, ExtensionInitError, ExtensionRegistrar,
    ModelRoute, ModelRouteReason, SessionEntry, SessionManager, ToolDefinition,
    VIRTUAL_MODEL_STATE_ENTRY, VirtualModelDefinition,
};
use serde_json::json;

struct RouterExtension {
    custom_provider: pi_ai::Provider,
    physical_model: pi_ai::Model,
}

impl Extension for RouterExtension {
    fn path(&self) -> &str {
        "<example:router>"
    }

    fn register(&self, api: &mut ExtensionRegistrar) -> Result<(), ExtensionInitError> {
        api.register_provider(self.custom_provider.clone());

        let phys = self.physical_model.clone();
        api.register_virtual_model(VirtualModelDefinition {
            provider: self.physical_model.provider.clone(),
            id: "auto-router".to_owned(),
            name: "Auto Router".to_owned(),
            thinking_levels: Some(vec![ModelThinkingLevel::Off, ModelThinkingLevel::High]),
            context_window: Some(128_000),
            max_tokens: Some(8_192),
            input: None,
            route: Arc::new(move |req| {
                let chosen = phys.clone();
                Box::pin(async move {
                    let prev_turns = req
                        .state
                        .as_ref()
                        .and_then(|v| v.get("routedTurns"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let next_turns = prev_turns + 1;
                    let selected_level = match req.reason {
                        ModelRouteReason::User => ModelThinkingLevel::Off,
                        ModelRouteReason::Continuation | ModelRouteReason::Retry => {
                            ModelThinkingLevel::High
                        }
                        ModelRouteReason::Direct => ModelThinkingLevel::Off,
                    };
                    Ok(ModelRoute {
                        model: chosen.clone(),
                        thinking_level: selected_level,
                        state: Some(json!({
                            "routedTurns": next_turns,
                            "lastReason": req.reason,
                            "lastPhysicalModel": chosen.id,
                        })),
                    })
                })
            }),
        });
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    let reg = create_faux_provider();
    let mut tc_args = JsonObject::new();
    tc_args.insert("q".to_owned(), json!("status"));
    reg.set_responses(vec![
        FauxResponseStep::Static(faux_assistant_message(
            vec![faux_tool_call("ping", tc_args, Some("call_1".to_owned()))],
            Some(StopReason::ToolUse),
        )),
        FauxResponseStep::Static(faux_assistant_message(
            vec![faux_text("routed continuation complete")],
            Some(StopReason::Stop),
        )),
    ]);

    let ping_tool = ToolDefinition::new(
        "ping",
        "Ping",
        "Ping tool",
        json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        Arc::new(|_id, _args, _cancel, _upd, _ctx| {
            Box::pin(async { Ok(AgentToolResult::text("pong")) })
        }),
    );

    let models = Arc::new(Models::default());
    let sm = SessionManager::in_memory(".", None, None).unwrap();
    let mut opts = AgentSessionOptions::new(".", reg.model.clone(), models.clone(), sm);
    opts.base_tools = vec![ping_tool];
    opts.extensions = vec![Box::new(RouterExtension {
        custom_provider: reg.provider.clone(),
        physical_model: reg.model.clone(),
    })];

    let session = AgentSession::new(opts).unwrap();
    let virtual_model = models
        .get_model(&reg.model.provider, "auto-router")
        .expect("virtual model registered in catalog");
    session.set_model(virtual_model).await.unwrap();

    session.prompt("run a routed check", None).await.unwrap();

    // 验证会话当前选择仍保持为虚拟模型 auto-router，而分支 CustomEntry 中持久化了路由状态
    assert_eq!(session.model().id, "auto-router");
    let branch = session.with_session_manager(|sm| sm.get_branch(None));
    let route_states: Vec<_> = branch
        .iter()
        .filter_map(|e| match e {
            SessionEntry::Custom(ce) if ce.custom_type == VIRTUAL_MODEL_STATE_ENTRY => {
                ce.data.clone()
            }
            _ => None,
        })
        .collect();
    assert_eq!(route_states.len(), 2);
    assert_eq!(route_states[0]["state"]["lastReason"], "user");
    assert_eq!(route_states[1]["state"]["lastReason"], "continuation");
    println!("example custom_provider_and_virtual_model passed");
}

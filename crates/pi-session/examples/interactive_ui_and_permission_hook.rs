use std::sync::{Arc, Mutex};

use pi_agent::AgentToolResult;
use pi_ai::faux::{
    FauxResponseStep, faux_assistant_message, faux_text, faux_tool_call, register_faux_provider,
};
use pi_ai::{JsonObject, Models, StopReason};
use pi_session::{
    AgentSession, AgentSessionOptions, ChannelUiContext, ExtensionMode, InlineExtension,
    SessionBeforeEventOutput, SessionLifecycleEvent, SessionManager, ToolCallEventResult,
    ToolDefinition, UiDialogOptions, UiNotifyLevel, UiPromptRequest,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() {
    let models = Arc::new(Models::default());
    let reg = register_faux_provider(&models);
    let mut unlock_args = JsonObject::new();
    unlock_args.insert("volume".to_owned(), json!("D:"));
    reg.set_responses(vec![
        FauxResponseStep::Static(faux_assistant_message(
            vec![faux_tool_call(
                "unlock_volume",
                unlock_args,
                Some("call_unlock".to_owned()),
            )],
            Some(StopReason::ToolUse),
        )),
        FauxResponseStep::Static(faux_assistant_message(
            vec![faux_text("Volume D: unlocked after user confirmation.")],
            Some(StopReason::Stop),
        )),
    ]);

    let prompt_events = Arc::new(Mutex::new(Vec::<String>::new()));
    let pe_start = prompt_events.clone();
    let pe_end = prompt_events.clone();

    let ext = InlineExtension::new("<example:ui-permission>", move |api| {
        let pe1 = pe_start.clone();
        api.on_lifecycle("ui_prompt_start", move |ev, _ctx| {
            let pe1 = pe1.clone();
            async move {
                if let SessionLifecycleEvent::UiPromptStart { kind, title } = ev {
                    pe1.lock()
                        .unwrap()
                        .push(format!("start:{kind:?}:{}", title.unwrap_or_default()));
                }
                Ok(SessionBeforeEventOutput::None)
            }
        });

        let pe2 = pe_end.clone();
        api.on_lifecycle("ui_prompt_end", move |ev, _ctx| {
            let pe2 = pe2.clone();
            async move {
                if let SessionLifecycleEvent::UiPromptEnd { kind, title } = ev {
                    pe2.lock()
                        .unwrap()
                        .push(format!("end:{kind:?}:{}", title.unwrap_or_default()));
                }
                Ok(SessionBeforeEventOutput::None)
            }
        });

        api.on_tool_call(|ev, ctx| async move {
            if ev.tool_name == "unlock_volume" {
                let confirmed = ctx
                    .ui()
                    .confirm(
                        "Confirm Volume Unlock",
                        "Allow unlocking volume D:?",
                        None,
                        CancellationToken::new(),
                    )
                    .await;
                if !confirmed {
                    return Ok(Some(ToolCallEventResult {
                        block: true,
                        reason: Some("User rejected unlock".to_owned()),
                    }));
                }

                let secret_key = ctx
                    .ui()
                    .input(
                        "Enter Recovery Password",
                        Some("48-digit recovery password"),
                        Some(UiDialogOptions {
                            secret: true,
                            ..UiDialogOptions::default()
                        }),
                        CancellationToken::new(),
                    )
                    .await;
                if secret_key.as_deref() != Some("111111-222222") {
                    return Ok(Some(ToolCallEventResult {
                        block: true,
                        reason: Some("Missing or invalid recovery password".to_owned()),
                    }));
                }
                ctx.ui()
                    .notify("Recovery password verified in memory", UiNotifyLevel::Info);
            }
            Ok(None)
        });

        api.register_tool(ToolDefinition::new(
            "unlock_volume",
            "Unlock Volume",
            "Unlock encrypted volume",
            json!({
                "type": "object",
                "properties": { "volume": { "type": "string" } },
                "required": ["volume"]
            }),
            Arc::new(|_id, args, _cancel, _upd, _ctx| {
                Box::pin(async move {
                    let vol = args["volume"].as_str().unwrap_or_default();
                    Ok(AgentToolResult::text(format!("unlocked {vol}")))
                })
            }),
        ));
        Ok(())
    });

    let (ui_ctx, mut ui_rx) = ChannelUiContext::pair();
    tokio::spawn(async move {
        while let Some(req) = ui_rx.recv().await {
            match req {
                UiPromptRequest::Confirm { respond, .. } => {
                    let _ = respond.send(true);
                }
                UiPromptRequest::Input { opts, respond, .. } => {
                    assert!(opts.as_ref().is_some_and(|o| o.secret));
                    let _ = respond.send(Some("111111-222222".to_owned()));
                }
                _ => {}
            }
        }
    });

    let sm = SessionManager::in_memory(".", None, None).unwrap();
    let mut opts = AgentSessionOptions::new(".", reg.model.clone(), models, sm);
    opts.extensions = vec![Box::new(ext)];
    opts.ui_context = Some(Arc::new(ui_ctx.clone()));
    opts.mode = ExtensionMode::Interactive;

    let session = AgentSession::new(opts).unwrap();
    session.prompt("unlock D:", None).await.unwrap();

    let events = prompt_events.lock().unwrap().clone();
    assert_eq!(
        events,
        vec![
            "start:Confirm:Confirm Volume Unlock".to_owned(),
            "end:Confirm:Confirm Volume Unlock".to_owned(),
            "start:Input:Enter Recovery Password".to_owned(),
            "end:Input:Enter Recovery Password".to_owned(),
        ]
    );
    assert_eq!(ui_ctx.snapshot().notifications.len(), 1);
    println!("example interactive_ui_and_permission_hook passed");
}

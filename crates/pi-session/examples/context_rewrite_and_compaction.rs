use std::sync::Arc;

use pi_ai::Models;
use pi_ai::faux::{FauxResponseStep, faux_text_message, register_faux_provider};
use pi_session::{
    AgentSession, AgentSessionOptions, BoundaryResult, CompactionResult, ContextEditReplacement,
    ContextEventResult, InlineExtension, InputEventResult, SessionBeforeCompactResult,
    SessionBeforeEventOutput, SessionBoundaryDraft, SessionEntry, SessionLifecycleEvent,
    SessionManager,
};
use serde_json::json;

#[tokio::main]
async fn main() {
    let models = Arc::new(Models::default());
    let reg = register_faux_provider(&models);
    reg.set_responses(vec![
        FauxResponseStep::Static(faux_text_message("First response with verbose details")),
        FauxResponseStep::Static(faux_text_message("Second response")),
    ]);

    let ext = InlineExtension::new("<example:context-and-compaction>", |api| {
        api.on_input(|ev, _ctx| async move {
            if ev.text.starts_with("rewrite:") {
                Ok(Some(InputEventResult::Transform {
                    text: ev.text.replacen("rewrite:", "[transformed] ", 1),
                    images: ev.images,
                }))
            } else {
                Ok(Some(InputEventResult::Continue))
            }
        });

        api.on_context(Arc::new(|ev, _ctx| {
            Box::pin(async move {
                let trimmed = ev.messages.last().cloned().into_iter().collect::<Vec<_>>();
                Ok(Some(ContextEventResult {
                    messages: Some(trimmed),
                }))
            })
        }));

        api.on_turn_end(|ev, ctx| async move {
            let branch = ctx.session_branch();
            let last_asst_id = branch.iter().rev().find_map(|e| match e {
                SessionEntry::Message(m) if m.message.role() == "assistant" => Some(m.id.clone()),
                _ => None,
            });
            let mut drafts = vec![SessionBoundaryDraft::Custom {
                custom_type: "example.turn-audit".to_owned(),
                data: Some(json!({ "estimatedTokens": ev.context.estimated_tokens })),
            }];
            if let Some(target_id) = last_asst_id {
                drafts.push(SessionBoundaryDraft::ContextEdit {
                    target_id,
                    replacement: Some(ContextEditReplacement::from_text(
                        "[edited by turn_end boundary draft]",
                    )),
                });
            }
            Ok(Some(BoundaryResult {
                entries: Some(drafts),
                r#continue: Some(false),
            }))
        });

        api.on_lifecycle("session_before_compact", |ev, _ctx| async move {
            if let SessionLifecycleEvent::SessionBeforeCompact { preparation, .. } = ev {
                return Ok(SessionBeforeEventOutput::Compact(
                    SessionBeforeCompactResult {
                        cancel: false,
                        compaction: Some(CompactionResult {
                            summary: "Custom extension checkpoint summary".to_owned(),
                            first_kept_entry_id: preparation.first_kept_entry_id,
                            tokens_before: preparation.tokens_before,
                            estimated_tokens_after: None,
                            usage: None,
                            details: Some(json!({ "custom": true })),
                        }),
                    },
                ));
            }
            Ok(SessionBeforeEventOutput::None)
        });

        Ok(())
    });

    let sm = SessionManager::in_memory(".", None, None).unwrap();
    let mut opts = AgentSessionOptions::new(".", reg.model.clone(), models, sm);
    opts.extensions = vec![Box::new(ext)];

    let session = AgentSession::new(opts).unwrap();
    session.prompt("rewrite:hello world", None).await.unwrap();
    session.prompt("second question", None).await.unwrap();

    let proj = session.with_session_manager(SessionManager::build_session_projection);
    assert_eq!(proj.messages.first().unwrap().role(), "system");
    let asst_texts: Vec<String> = proj
        .messages
        .iter()
        .filter_map(|m| m.as_assistant().map(pi_ai::AssistantMessage::text))
        .collect();
    assert!(
        asst_texts
            .iter()
            .all(|t| t == "[edited by turn_end boundary draft]")
    );

    let comp = session.compact(None).await.unwrap();
    assert_eq!(comp.summary, "Custom extension checkpoint summary");
    println!("example context_rewrite_and_compaction passed");
}

use pi_session::*;
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn before_agent_start_context_must_reflect_current_handler_mutation() {
    let observed = Arc::new(Mutex::new(None));
    let observed_hook = observed.clone();
    let ext = InlineExtension::new("probe", move |api| {
        let observed = observed_hook.clone();
        api.on_before_agent_start(Arc::new(move |ev, ctx| {
            let observed = observed.clone();
            Box::pin(async move {
                ev.system_prompt_options.update(|options| {
                    options.force_system_prompt = Some("changed-inside-hook".into());
                });
                tokio::task::yield_now().await;
                let event_value = ev.system_prompt();
                let context_value = ctx.get_system_prompt();
                *observed.lock().unwrap() = Some((event_value, context_value));
                Ok(None)
            })
        }));
        Ok(())
    });
    let runner = ExtensionRunner::from_extensions("/probe", vec![Box::new(ext)]).unwrap();
    let result = runner
        .emit_before_agent_start(
            BeforeAgentStartTrigger::User {
                prompt: "test".into(),
                images: None,
            },
            &BuildSystemPromptOptions::default(),
        )
        .await;
    assert_eq!(
        result.system_prompt_options.force_system_prompt.as_deref(),
        Some("changed-inside-hook")
    );
    let (event_value, context_value) = observed.lock().unwrap().take().unwrap();
    assert_eq!(
        event_value, context_value,
        "ctx sees stale system prompt while event sees updated prompt"
    );
}

#[tokio::test]
async fn control_before_agent_start_next_handler_sees_committed_change() {
    let seen = Arc::new(Mutex::new(None));
    let read_seen = seen.clone();
    let ext = InlineExtension::new("probe-control", move |api| {
        api.on_before_agent_start(Arc::new(|ev, _| {
            Box::pin(async move {
                ev.system_prompt_options.update(|options| {
                    options.force_system_prompt = Some("changed".into());
                });
                Ok(None)
            })
        }));
        let seen = read_seen.clone();
        api.on_before_agent_start(Arc::new(move |_, ctx| {
            *seen.lock().unwrap() = Some(ctx.get_system_prompt());
            Box::pin(async { Ok(None) })
        }));
        Ok(())
    });
    let runner = ExtensionRunner::from_extensions("/probe", vec![Box::new(ext)]).unwrap();
    runner
        .emit_before_agent_start(
            BeforeAgentStartTrigger::User {
                prompt: "test".into(),
                images: None,
            },
            &BuildSystemPromptOptions::default(),
        )
        .await;
    assert_eq!(seen.lock().unwrap().as_deref(), Some("changed"));
}

#[tokio::test]
async fn prompt_updates_survive_handler_errors_and_return_override_wins() {
    let ext = InlineExtension::new("prompt-updates", |api| {
        api.on_before_agent_start(Arc::new(|ev, ctx| {
            Box::pin(async move {
                ev.system_prompt_options.update(|options| {
                    // Rendering in the synchronous update callback must not deadlock.
                    assert!(!ctx.get_system_prompt().is_empty());
                    options.force_system_prompt = Some("before-error".into());
                });
                Err("handler failure after update".into())
            })
        }));
        api.on_before_agent_start(Arc::new(|ev, ctx| {
            Box::pin(async move {
                assert_eq!(ctx.get_system_prompt(), "before-error");
                assert_eq!(
                    ev.system_prompt_options
                        .snapshot()
                        .force_system_prompt
                        .as_deref(),
                    Some("before-error")
                );
                Ok(Some(BeforeAgentStartEventResult {
                    system_prompt: Some("returned-override".into()),
                    ..Default::default()
                }))
            })
        }));
        api.on_before_agent_start(Arc::new(|ev, ctx| {
            Box::pin(async move {
                assert_eq!(ev.system_prompt(), "returned-override");
                assert_eq!(ctx.get_system_prompt(), "returned-override");
                Ok(None)
            })
        }));
        Ok(())
    });
    let runner = ExtensionRunner::from_extensions("/probe", vec![Box::new(ext)]).unwrap();
    let result = runner
        .emit_before_agent_start(
            BeforeAgentStartTrigger::User {
                prompt: "test".into(),
                images: None,
            },
            &BuildSystemPromptOptions::default(),
        )
        .await;
    assert_eq!(
        result.system_prompt_options.force_system_prompt.as_deref(),
        Some("returned-override")
    );
}

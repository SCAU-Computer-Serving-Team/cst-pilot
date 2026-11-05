use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use pi_agent::*;
use pi_ai::{Model, StopReason, UserMessage};
use tokio_util::sync::CancellationToken;

fn model() -> Model {
    pi_ai::faux::create_faux_provider().model
}

#[tokio::test]
async fn stateful_agent_settles_provider_panic() {
    let stream_fn: StreamFn = Arc::new(|_, _, _| Box::pin(async { panic!("provider failure") }));
    let agent = Agent::new(AgentOptions::new(stream_fn));
    let result = tokio::time::timeout(Duration::from_secs(1), agent.prompt("hello")).await;
    assert!(result.unwrap().is_err());
    assert!(!agent.state().is_streaming);
}

#[tokio::test]
async fn stateless_agent_provider_panics_close_events_and_result() {
    for continuing in [false, true] {
        for synchronous in [false, true] {
            for cancelled in [false, true] {
                let token = CancellationToken::new();
                let callback_token = token.clone();
                let stream_fn: StreamFn = Arc::new(move |_, _, _| {
                    if cancelled {
                        callback_token.cancel();
                    }
                    assert!(!synchronous, "provider failure");
                    Box::pin(async { panic!("provider failure") })
                });
                let user = AgentMessage::User(UserMessage::new("hello"));
                let mut stream = if continuing {
                    agent_loop_continue(
                        AgentContext {
                            messages: vec![user],
                            ..Default::default()
                        },
                        AgentLoopConfig::new(model()),
                        Some(token),
                        stream_fn,
                    )
                    .unwrap()
                } else {
                    agent_loop(
                        vec![user],
                        AgentContext::default(),
                        AgentLoopConfig::new(model()),
                        Some(token),
                        stream_fn,
                    )
                };
                let (events, result) = tokio::time::timeout(Duration::from_secs(1), async {
                    let mut events = Vec::new();
                    while let Some(event) = stream.next().await {
                        events.push(event);
                    }
                    (events, stream.result().await)
                })
                .await
                .expect("provider panic left stateless stream unfinished");
                let AgentMessage::Assistant(message) = result.last().unwrap() else {
                    panic!("missing error assistant message");
                };
                assert_eq!(
                    message.stop_reason,
                    if cancelled {
                        StopReason::Aborted
                    } else {
                        StopReason::Error
                    }
                );
                assert!(
                    message
                        .error_message
                        .as_deref()
                        .unwrap()
                        .contains("provider failure")
                );
                assert_eq!(
                    events
                        .iter()
                        .filter(|e| matches!(e, AgentEvent::AgentEnd { .. }))
                        .count(),
                    1
                );
                assert_eq!(
                    events
                        .iter()
                        .filter(|e| matches!(e, AgentEvent::TurnEnd { .. }))
                        .count(),
                    1
                );
                let tail = &events[events.len() - 4..];
                assert!(matches!(tail[0], AgentEvent::MessageStart { .. }));
                assert!(matches!(tail[1], AgentEvent::MessageEnd { .. }));
                assert!(matches!(tail[2], AgentEvent::TurnEnd { .. }));
                assert!(
                    matches!(&tail[3], AgentEvent::AgentEnd { messages } if messages == &result)
                );
            }
        }
    }
}

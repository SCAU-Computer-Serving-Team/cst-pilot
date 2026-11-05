use futures_util::StreamExt;
use pi_ai::{AssistantMessageEvent, StopReason, create_assistant_message_event_stream};

#[tokio::test]
async fn waiting_for_result_must_not_destroy_stream_events() {
    let stream = create_assistant_message_event_stream();
    let message = pi_ai::faux::faux_text_message("hello");
    stream.push(AssistantMessageEvent::Start {
        partial: message.clone(),
    });
    stream.push(AssistantMessageEvent::Done {
        reason: StopReason::Stop,
        message,
    });
    let _ = stream.result().await;
    let events = stream.collect::<Vec<_>>().await;
    assert_eq!(events.len(), 2, "result() silently erased queued events");
}

#[tokio::test]
async fn pending_result_waiters_preserve_subsequent_events() {
    let stream = create_assistant_message_event_stream();
    let reader = stream.clone();
    let first = reader.result();
    let second = reader.result();
    tokio::pin!(first, second);
    assert!(futures_util::poll!(&mut first).is_pending());
    assert!(futures_util::poll!(&mut second).is_pending());
    let message = pi_ai::faux::faux_text_message("hello");
    stream.push(AssistantMessageEvent::Start {
        partial: message.clone(),
    });
    stream.push(AssistantMessageEvent::Done {
        reason: StopReason::Stop,
        message,
    });
    let (first, second, events) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(first, second, stream.collect::<Vec<_>>())
    })
    .await
    .unwrap();
    assert_eq!(first, second);
    assert_eq!(events.len(), 2);
    assert_eq!(reader.result().await, first);
}

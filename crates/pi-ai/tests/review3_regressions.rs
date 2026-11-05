use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_ai::*;

#[tokio::test]
async fn anthropic_thinking_preserves_context_output_cap() {
    for used_tokens in [1_000, 190_000, 199_000] {
        let mut model = faux::create_faux_provider().model;
        model.provider = "anthropic".into();
        model.api = "anthropic-messages".into();
        model.context_window = 200_000;
        model.max_tokens = 64_000;
        model.reasoning = true;
        model.compat = None;
        let mut previous = faux::faux_text_message("previous answer");
        previous.usage.total_tokens = used_tokens;
        let context = normalize_context(&Context {
            messages: vec![
                Message::Assistant(previous),
                Message::User(UserMessage::new("continue")),
            ],
            ..Default::default()
        });
        let captured = Arc::new(Mutex::new(None));
        let cancel = tokio_util::sync::CancellationToken::new();
        let options = SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::High),
            base: StreamOptions {
                api_key: Some("local-probe-key".into()),
                cancel_token: Some(cancel.clone()),
                on_payload: Some(Arc::new({
                    let captured = captured.clone();
                    move |payload, _| {
                        *captured.lock().unwrap() = Some(payload);
                        cancel.cancel(); // Stop before any network traffic.
                        Box::pin(async { None })
                    }
                })),
                ..Default::default()
            },
            ..Default::default()
        };
        let cap = api::build_base_options(&model, &context, Some(&options))
            .max_tokens
            .unwrap();
        let stream = api::anthropic_messages::stream_simple(&model, &context, Some(&options));
        let result = tokio::time::timeout(Duration::from_secs(1), stream.result())
            .await
            .unwrap();
        assert_eq!(result.stop_reason, StopReason::Aborted);
        let payload = captured.lock().unwrap().take().unwrap();
        let sent_max = payload["max_tokens"].as_u64().unwrap();
        assert_eq!(sent_max, cap);
        if let Some(budget) = payload
            .pointer("/thinking/budget_tokens")
            .and_then(JsonValue::as_u64)
        {
            assert!(budget >= 1024);
            assert!(budget + 1024 <= sent_max);
        } else {
            assert!(sent_max < 2048);
        }
    }
}

use std::collections::BTreeMap;

use pi_agent::{ProxyAssistantMessageEvent, default_agent_model, process_proxy_event};
use pi_ai::{AssistantMessage, ContentBlock};

#[test]
fn proxy_rejects_sparse_content_indices_without_changing_partial() {
    // A small gap reproduces the allocation pattern without exhausting memory.
    for event in [
        ProxyAssistantMessageEvent::TextStart { content_index: 100 },
        ProxyAssistantMessageEvent::ThinkingStart { content_index: 100 },
        ProxyAssistantMessageEvent::ToolCallStart {
            content_index: 100,
            id: "call-1".into(),
            tool_name: "read".into(),
        },
    ] {
        let mut partial = AssistantMessage::new_pending(&default_agent_model());
        let mut arguments = BTreeMap::new();
        let result = process_proxy_event(event, &mut partial, &mut arguments);
        assert!(result.is_err(), "sparse index was accepted");
        assert!(partial.content.is_empty());
        assert!(arguments.is_empty());
    }
}

#[test]
fn proxy_accepts_contiguous_and_existing_content_indices() {
    let mut partial = AssistantMessage::new_pending(&default_agent_model());
    let mut arguments = BTreeMap::new();
    for event in [
        ProxyAssistantMessageEvent::TextStart { content_index: 0 },
        ProxyAssistantMessageEvent::ThinkingStart { content_index: 1 },
        ProxyAssistantMessageEvent::TextStart { content_index: 0 },
    ] {
        process_proxy_event(event, &mut partial, &mut arguments).unwrap();
    }
    assert_eq!(partial.content.len(), 2);
    assert!(matches!(partial.content[0], ContentBlock::Text(_)));
    assert!(matches!(partial.content[1], ContentBlock::Thinking(_)));
}

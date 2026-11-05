use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use pi_agent::{
    AgentContext, AgentTool, AgentToolResult, RunToolCallOptions, ToolUpdateSink,
    default_agent_model, run_tool_call,
};
use pi_ai::{AssistantMessage, ToolCall};
use serde_json::json;

fn call() -> ToolCall {
    ToolCall {
        id: "call".to_owned(),
        name: "probe".to_owned(),
        arguments: Default::default(),
        thought_signature: None,
        namespace: None,
        arguments_error: None,
    }
}

fn options(tool: AgentTool, on_update: Option<ToolUpdateSink>) -> RunToolCallOptions {
    RunToolCallOptions {
        tools: vec![tool],
        assistant_message: AssistantMessage::new_pending(&default_agent_model()),
        context: AgentContext::default(),
        cancel_token: None,
        on_update,
        before_tool_call: None,
        after_tool_call: None,
    }
}

#[tokio::test]
async fn panicking_argument_preprocessor_returns_a_tool_error_without_execution() {
    let mut tool = AgentTool::new(
        "probe",
        "Probe",
        "Probe",
        json!({"type": "object"}),
        Arc::new(|_, _, _, _| panic!("must not execute after preprocessing failed")),
    );
    tool.prepare_arguments = Some(Arc::new(|_| panic!("injected preprocessing panic")));
    let outcome = run_tool_call(call(), options(tool, None)).await;
    assert!(outcome.is_error);
    assert!(outcome.result.is_error);
    assert_eq!(outcome.duration_ms, None);
    let text = serde_json::to_string(&outcome.result.content).unwrap();
    assert!(text.contains("prepare_arguments panicked"));
}

#[tokio::test]
async fn synchronous_and_async_update_callback_panics_do_not_break_tool_settlement() {
    for synchronous in [true, false] {
        let delivered = Arc::new(AtomicUsize::new(0));
        let counter = delivered.clone();
        let sink: ToolUpdateSink = Arc::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            assert!(!synchronous, "injected synchronous update panic");
            Box::pin(async { panic!("injected async update panic") })
        });
        let tool = AgentTool::new(
            "probe",
            "Probe",
            "Probe",
            json!({"type": "object"}),
            Arc::new(|_, _, _, on_update| {
                Box::pin(async move {
                    let on_update = on_update.unwrap();
                    on_update(AgentToolResult::text("first"));
                    tokio::task::yield_now().await;
                    on_update(AgentToolResult::text("second"));
                    Ok(AgentToolResult::text("done"))
                })
            }),
        );
        let outcome = tokio::time::timeout(
            Duration::from_secs(2),
            run_tool_call(call(), options(tool, Some(sink))),
        )
        .await
        .unwrap();
        assert!(!outcome.is_error);
        assert!(outcome.duration_ms.is_some());
        assert_eq!(delivered.load(Ordering::SeqCst), 2);
    }
}

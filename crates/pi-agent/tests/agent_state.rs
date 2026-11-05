use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_agent::{
    Agent, AgentEvent, AgentInitialState, AgentMessage, AgentOptions, AgentTool, AgentToolResult,
    QueueMode, StreamFn,
};
use pi_ai::faux::{faux_assistant_message, faux_text_message, faux_tool_call};
use pi_ai::{
    AssistantMessage, AssistantMessageEvent, JsonObject, StopReason, SystemContent, SystemMessage,
    UserMessage, create_assistant_message_event_stream, now_ms,
};
use serde_json::json;

fn make_scripted_stream(steps: Vec<AssistantMessage>) -> (StreamFn, Arc<AtomicUsize>) {
    let queue = Arc::new(Mutex::new(std::collections::VecDeque::from(steps)));
    let count = Arc::new(AtomicUsize::new(0));
    let count_clone = count.clone();

    let stream_fn: StreamFn = Arc::new(move |model, _ctx, _opts| {
        let queue = queue.clone();
        let count_clone = count_clone.clone();
        Box::pin(async move {
            count_clone.fetch_add(1, Ordering::SeqCst);
            let stream = create_assistant_message_event_stream();
            let writer = stream.clone();
            let mut msg = queue
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| faux_text_message("fallback"));
            msg.model = model.id.clone();
            msg.provider = model.provider.clone();
            msg.api = model.api.clone();

            tokio::spawn(async move {
                let mut partial = AssistantMessage::new_pending(&model);
                writer.push(AssistantMessageEvent::Start {
                    partial: partial.clone(),
                });
                partial.content = msg.content.clone();
                writer.push(AssistantMessageEvent::Done {
                    reason: msg.stop_reason,
                    message: msg,
                });
                writer.end(None);
            });
            stream
        })
    });

    (stream_fn, count)
}

#[tokio::test]
async fn initial_state_seeds_system_message_and_reset_retains_replayed_baseline() {
    let tool = AgentTool::new(
        "lookup",
        "Lookup",
        "Looks up info",
        json!({"type": "object"}),
        Arc::new(|_, _, _, _| Box::pin(async { Ok(AgentToolResult::text("found")) })),
    );
    let (stream_fn, _) = make_scripted_stream(vec![faux_text_message("hello user")]);

    let mut opts = AgentOptions::new(stream_fn);
    opts.initial_state = Some(AgentInitialState {
        system_prompt: Some("Base instructions".to_owned()),
        model: None,
        thinking_level: None,
        tools: Some(vec![tool]),
        messages: None,
    });

    let agent = Agent::new(opts);
    assert_eq!(agent.state().system_prompt, "Base instructions");
    assert_eq!(agent.state().messages.len(), 1);
    assert_eq!(agent.state().messages[0].role(), "system");

    agent.prompt("Hi").await.expect("prompt should succeed");
    assert_eq!(agent.state().messages.len(), 3); // system + user + assistant

    // Append a mid-conversation system section update, then reset
    let mut msgs = agent.state().messages;
    msgs.push(AgentMessage::System(SystemMessage {
        content: SystemContent::Text(String::new()),
        sections: Some(BTreeMap::from([(
            "rules".to_owned(),
            Some("Be concise.".to_owned()),
        )])),
        tools_added: None,
        tools_removed: None,
        timestamp: now_ms(),
    }));
    agent.set_messages(msgs);
    assert_eq!(
        agent.state().system_prompt,
        "Base instructions\n\nBe concise."
    );

    agent.steer(UserMessage::new("queued steer"));
    agent.follow_up(UserMessage::new("queued follow"));
    assert!(agent.has_queued_messages());

    agent.reset().expect("reset should succeed when idle");
    assert!(!agent.has_queued_messages());
    assert_eq!(agent.state().messages.len(), 1);
    assert_eq!(
        agent.state().system_prompt,
        "Base instructions\n\nBe concise."
    );
}

#[tokio::test]
async fn continue_run_handles_empty_system_only_user_tail_and_assistant_tail_queue_fallbacks() {
    let (stream_fn, call_count) = make_scripted_stream(vec![
        faux_text_message("reply 1"),
        faux_text_message("reply 2 from steer"),
        faux_text_message("reply 3 from second steer"),
        faux_text_message("reply 4 from follow_up"),
    ]);

    let mut opts = AgentOptions::new(stream_fn);
    opts.initial_state = Some(AgentInitialState {
        system_prompt: Some("Sys".to_owned()),
        ..Default::default()
    });
    let agent = Agent::new(opts);

    // 1. System-only transcript -> fails with "No messages to continue from" without draining queues
    agent.steer(UserMessage::new("s1"));
    let err = agent.continue_run().await.unwrap_err();
    assert_eq!(err, "No messages to continue from");
    assert_eq!(agent.peek_queued_messages().len(), 1);
    agent.clear_all_queues();

    // 2. Normal prompt leaves transcript ending in Assistant
    agent.prompt("initial").await.unwrap();
    assert_eq!(call_count.load(Ordering::SeqCst), 1);

    // 3. Assistant tail with empty queues -> fails with "Cannot continue from message role: assistant"
    let err2 = agent.continue_run().await.unwrap_err();
    assert_eq!(err2, "Cannot continue from message role: assistant");

    // 4. Assistant tail with 2 steering messages in OneAtATime mode ->
    // First steering is used as prompt (with skip_initial_steering_poll = true),
    // second steering is picked up at the end of turn 1!
    agent.set_steering_mode(QueueMode::OneAtATime);
    agent.steer(UserMessage::new("steer_A"));
    agent.steer(UserMessage::new("steer_B"));
    agent.continue_run().await.unwrap();
    assert_eq!(call_count.load(Ordering::SeqCst), 3);
    assert!(!agent.has_queued_messages());

    // 5. Assistant tail with follow_up message -> drains follow_up and runs
    agent.follow_up(UserMessage::new("follow_C"));
    agent.continue_run().await.unwrap();
    assert_eq!(call_count.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn listeners_and_hooks_can_reenter_agent_without_deadlock_and_wait_for_idle_waits_for_agent_end_listener()
 {
    let tool = AgentTool::new(
        "echo",
        "Echo",
        "Echoes",
        json!({"type": "object"}),
        Arc::new(|_, _, _, _| Box::pin(async { Ok(AgentToolResult::text("echoed")) })),
    );

    let (stream_fn, call_count) = make_scripted_stream(vec![
        faux_assistant_message(
            vec![faux_tool_call(
                "echo",
                JsonObject::new(),
                Some("c1".to_owned()),
            )],
            Some(StopReason::ToolUse),
        ),
        faux_text_message("after tool"),
        faux_text_message("after follow up"),
    ]);

    let mut opts = AgentOptions::new(stream_fn);
    opts.initial_state = Some(AgentInitialState {
        tools: Some(vec![tool]),
        ..Default::default()
    });
    let agent = Agent::new(opts);

    // Hook re-entrantly reads agent.state() and enqueues a follow_up message
    let agent_for_hook = agent.clone();
    agent.set_before_tool_call(Some(Arc::new(move |_ctx, _cancel| {
        let agent = agent_for_hook.clone();
        Box::pin(async move {
            // Must not deadlock!
            let state = agent.state();
            assert!(state.is_streaming);
            // Assistant message that issued the tool call is already in state.messages
            assert_eq!(state.messages.last().unwrap().role(), "assistant");
            agent.follow_up(UserMessage::new("hook follow up"));
            Ok(None)
        })
    })));

    let agent_end_listener_finished = Arc::new(AtomicBool::new(false));
    let flag_for_listener = agent_end_listener_finished.clone();
    let agent_for_listener = agent.clone();

    let _unsub = agent.subscribe(Arc::new(move |event, _cancel| {
        let flag = flag_for_listener.clone();
        let agent = agent_for_listener.clone();
        Box::pin(async move {
            // Re-entrant state() read inside listener must never deadlock
            let _ = agent.state();
            if matches!(event, AgentEvent::AgentEnd { .. }) {
                tokio::time::sleep(Duration::from_millis(40)).await;
                flag.store(true, Ordering::SeqCst);
            }
        })
    }));

    let agent_runner = agent.clone();
    let run_handle = tokio::spawn(async move {
        agent_runner.prompt("start").await.unwrap();
    });

    // Wait briefly until run starts
    tokio::time::sleep(Duration::from_millis(10)).await;
    agent.wait_for_idle().await;
    run_handle.await.unwrap();

    // When wait_for_idle() returns, the async AgentEnd listener must have already settled
    assert!(agent_end_listener_finished.load(Ordering::SeqCst));
    assert!(!agent.state().is_streaming);
    assert_eq!(call_count.load(Ordering::SeqCst), 3);
}

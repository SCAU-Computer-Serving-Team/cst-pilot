use std::sync::TryLockError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::*;

fn agent() -> Agent {
    Agent::new(AgentOptions::new(Arc::new(|_, _, _| {
        Box::pin(std::future::pending())
    })))
}

fn active_run() -> ActiveRun {
    ActiveRun {
        run_id: 1,
        cancel_token: CancellationToken::new(),
        idle_notify: Arc::new(Notify::new()),
        saw_agent_start: true,
        turn_open: false,
        saw_agent_end: true,
    }
}

#[test]
fn finishing_run_keeps_admission_closed_until_state_cleanup_completes() {
    let agent = agent();
    *agent.inner.active_run.lock().unwrap() = Some(active_run());
    let mut state = agent.inner.state.write().unwrap();
    state.is_streaming = true;
    state.pending_tool_calls.insert("old-call".to_owned());
    let worker = agent.clone();
    let thread = std::thread::spawn(move || worker.finish_run(1));
    let deadline = Instant::now() + Duration::from_secs(2);
    let kept_admission_closed = loop {
        match agent.inner.active_run.try_lock() {
            Err(TryLockError::WouldBlock) => break true,
            Err(TryLockError::Poisoned(_)) => panic!("poisoned admission lock"),
            Ok(guard) if guard.is_none() => break false,
            Ok(_) => {}
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::yield_now();
    };
    drop(state);
    thread.join().unwrap();
    assert!(
        kept_admission_closed,
        "run was released before clearing its state"
    );
    assert!(agent.inner.active_run.lock().unwrap().is_none());
    let state = agent.state();
    assert!(!state.is_streaming);
    assert!(state.pending_tool_calls.is_empty());
}

#[test]
fn reset_holds_admission_while_waiting_to_clear_state() {
    let agent = agent();
    let state = agent.inner.state.write().unwrap();
    let worker = agent.clone();
    let thread = std::thread::spawn(move || worker.reset());
    let deadline = Instant::now() + Duration::from_secs(2);
    let kept_admission_closed = loop {
        match agent.inner.active_run.try_lock() {
            Err(TryLockError::WouldBlock) => break true,
            Err(TryLockError::Poisoned(_)) => panic!("poisoned admission lock"),
            Ok(_) => {}
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::yield_now();
    };
    drop(state);
    thread.join().unwrap().unwrap();
    assert!(
        kept_admission_closed,
        "reset allowed a run to start during state cleanup"
    );
}

#[tokio::test]
async fn rejected_run_never_prepares_or_consumes_queued_messages() {
    let agent = agent();
    *agent.inner.active_run.lock().unwrap() = Some(active_run());
    agent.steer(AgentMessage::User(pi_ai::UserMessage {
        content: pi_ai::UserContent::Text("keep queued".to_owned()),
        timestamp: 0,
    }));
    let prepared = Arc::new(AtomicBool::new(false));
    let flag = prepared.clone();
    assert!(
        agent
            .run_with_lifecycle(move || {
                flag.store(true, Ordering::SeqCst);
                Ok(|_, _| async { Ok(()) })
            })
            .await
            .is_err()
    );
    assert!(!prepared.load(Ordering::SeqCst));
    assert!(agent.continue_run().await.is_err());
    assert_eq!(
        agent.inner.queues.lock().unwrap().steering.messages.len(),
        1
    );
}

#[tokio::test]
async fn invalid_continuation_does_not_claim_a_run_or_mark_streaming() {
    let agent = agent();
    assert_eq!(
        agent.continue_run().await.unwrap_err(),
        "No messages to continue from"
    );
    assert!(agent.inner.active_run.lock().unwrap().is_none());
    assert!(!agent.state().is_streaming);
}

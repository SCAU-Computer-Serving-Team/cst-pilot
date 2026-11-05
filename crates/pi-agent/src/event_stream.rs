use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll, Waker};

use futures_core::Stream;
use tokio::sync::Notify;

use crate::types::{AgentEvent, AgentMessage};

struct EventStreamState<E, R> {
    queue: VecDeque<E>,
    done: bool,
    final_result: Option<R>,
    waker: Option<Waker>,
}

struct EventStreamInner<E, R> {
    is_complete: Box<dyn Fn(&E) -> bool + Send + Sync>,
    extract_result: Box<dyn Fn(&E) -> R + Send + Sync>,
    state: Mutex<EventStreamState<E, R>>,
    result_notify: Notify,
}

/// 通用异步事件流（对照 `@earendil-works/pi-ai` 的 `EventStream<T, R>`）。
///
/// 既可作为 `futures_core::Stream<Item = E>` 逐条消费事件，
/// 也可直接调用 `stream.result().await` 等待最终结果 `R`。
pub struct EventStream<E, R = E> {
    inner: Arc<EventStreamInner<E, R>>,
}

impl<E, R> Clone for EventStream<E, R> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<E, R> EventStream<E, R> {
    pub fn new(
        is_complete: impl Fn(&E) -> bool + Send + Sync + 'static,
        extract_result: impl Fn(&E) -> R + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner: Arc::new(EventStreamInner {
                is_complete: Box::new(is_complete),
                extract_result: Box::new(extract_result),
                state: Mutex::new(EventStreamState {
                    queue: VecDeque::new(),
                    done: false,
                    final_result: None,
                    waker: None,
                }),
                result_notify: Notify::new(),
            }),
        }
    }

    pub fn push(&self, event: E) {
        let mut guard = self.inner.state.lock().expect("event stream lock poisoned");
        if guard.done {
            return;
        }
        if (self.inner.is_complete)(&event) {
            guard.done = true;
            if guard.final_result.is_none() {
                guard.final_result = Some((self.inner.extract_result)(&event));
                self.inner.result_notify.notify_waiters();
            }
        }
        guard.queue.push_back(event);
        if let Some(waker) = guard.waker.take() {
            waker.wake();
        }
    }

    pub fn end(&self, result: Option<R>) {
        let mut guard = self.inner.state.lock().expect("event stream lock poisoned");
        guard.done = true;
        if let Some(res) = result
            && guard.final_result.is_none()
        {
            guard.final_result = Some(res);
            self.inner.result_notify.notify_waiters();
        }
        if let Some(waker) = guard.waker.take() {
            waker.wake();
        }
    }
}

impl<E, R: Clone> EventStream<E, R> {
    pub async fn result(&self) -> R {
        loop {
            let notified = self.inner.result_notify.notified();
            {
                let guard = self.inner.state.lock().expect("event stream lock poisoned");
                if let Some(res) = &guard.final_result {
                    return res.clone();
                }
            }
            notified.await;
        }
    }
}

impl<E, R> Stream for EventStream<E, R> {
    type Item = E;

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        let mut guard = self.inner.state.lock().expect("event stream lock poisoned");
        if let Some(item) = guard.queue.pop_front() {
            return Poll::Ready(Some(item));
        }
        if guard.done {
            return Poll::Ready(None);
        }
        guard.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

pub type AgentEventStream = EventStream<AgentEvent, Vec<AgentMessage>>;

pub fn create_agent_event_stream() -> AgentEventStream {
    EventStream::new(
        |event| matches!(event, AgentEvent::AgentEnd { .. }),
        |event| match event {
            AgentEvent::AgentEnd { messages } => messages.clone(),
            _ => Vec::new(),
        },
    )
}

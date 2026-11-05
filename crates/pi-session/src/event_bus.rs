use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pi_ai::JsonValue;

pub type EventBusHandler = Arc<dyn Fn(JsonValue) + Send + Sync>;
type ChannelListenerMap = BTreeMap<String, Vec<(u64, EventBusHandler)>>;

/// 跨扩展通信事件总线（对照 `EventBus` / `EventBusController`）。
#[derive(Clone, Default)]
pub struct EventBus {
    listeners: Arc<Mutex<ChannelListenerMap>>,
    next_id: Arc<AtomicU64>,
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus").finish()
    }
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn emit(&self, channel: &str, data: JsonValue) {
        let handlers: Vec<EventBusHandler> = {
            let guard = self.listeners.lock().expect("event_bus lock poisoned");
            guard
                .get(channel)
                .map(|list| list.iter().map(|(_, h)| h.clone()).collect())
                .unwrap_or_default()
        };
        for handler in handlers {
            let payload = data.clone();
            let _ = catch_unwind(AssertUnwindSafe(|| handler(payload)));
        }
    }

    pub fn on(
        &self,
        channel: impl Into<String>,
        handler: EventBusHandler,
    ) -> Box<dyn FnOnce() + Send + Sync> {
        let channel = channel.into();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut guard = self.listeners.lock().expect("event_bus lock poisoned");
            guard
                .entry(channel.clone())
                .or_default()
                .push((id, handler));
        }
        let listeners = self.listeners.clone();
        Box::new(move || {
            if let Ok(mut guard) = listeners.lock()
                && let Some(list) = guard.get_mut(&channel)
            {
                list.retain(|(entry_id, _)| *entry_id != id);
            }
        })
    }

    pub fn clear(&self) {
        if let Ok(mut guard) = self.listeners.lock() {
            guard.clear();
        }
    }
}

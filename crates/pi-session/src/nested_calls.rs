use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use pi_agent::{
    AgentEvent, AgentTool, AgentToolCallOutcome, AgentToolResult, AgentToolUpdateCallback,
    BoxFuture, ToolExecutionMode, ToolUpdateSink,
};
use pi_ai::{JsonValue, NestedToolCallRecord, NestedToolCalls, ToolCall, Usage, UserContentBlock};
use serde_json::Map;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::usage_totals::combine_usage;

pub const NESTED_MAX_CALLS: usize = 256;
pub const NESTED_MAX_ARGUMENT_BYTES_PER_CALL: usize = 8 * 1024;
pub const NESTED_MAX_ARGUMENT_BYTES_TOTAL: usize = 32 * 1024;
pub const NESTED_MAX_ERROR_CHARS: usize = 500;

#[derive(Debug, Clone, Default)]
pub struct NestedCallSummary {
    pub calls: Option<NestedToolCalls>,
    pub usage: Option<Usage>,
}

struct RecordedCallEntry {
    record: NestedToolCallRecord,
    started_at: Instant,
}

/// 收集单个顶层工具调用派生的全部嵌套工具调用轨迹与累计 `Usage`（对照 `NestedCallRecorder`）。
pub struct NestedCallRecorder {
    calls: Vec<RecordedCallEntry>,
    complete: bool,
    argument_bytes: usize,
    usage: Option<Usage>,
}

impl Default for NestedCallRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl NestedCallRecorder {
    pub fn new() -> Self {
        Self {
            calls: Vec::new(),
            complete: true,
            argument_bytes: 0,
            usage: None,
        }
    }

    /// 登记一次嵌套调用启动；若超过 `max_calls (256)` 则丢弃并将 `complete` 标为 `false`。
    pub fn start(&mut self, tool_call: &ToolCall) -> Option<usize> {
        if self.calls.len() >= NESTED_MAX_CALLS {
            self.complete = false;
            return None;
        }
        let json_str =
            serde_json::to_string(&tool_call.arguments).unwrap_or_else(|_| "{}".to_owned());
        let bytes = json_str.len();

        let mut record = NestedToolCallRecord {
            id: tool_call.id.clone(),
            name: tool_call.name.clone(),
            arguments: None,
            arguments_bytes: None,
            status: "unfinished".to_owned(),
            duration_ms: None,
            error: None,
        };

        if bytes > NESTED_MAX_ARGUMENT_BYTES_PER_CALL
            || self.argument_bytes + bytes > NESTED_MAX_ARGUMENT_BYTES_TOTAL
        {
            record.arguments_bytes = Some(bytes);
            self.complete = false;
        } else {
            record.arguments = Some(tool_call.arguments.clone());
            self.argument_bytes += bytes;
        }

        let idx = self.calls.len();
        self.calls.push(RecordedCallEntry {
            record,
            started_at: Instant::now(),
        });
        Some(idx)
    }

    pub fn finish(&mut self, record_idx: Option<usize>, is_error: bool, error_text: &str) {
        let Some(idx) = record_idx else {
            return;
        };
        let Some(entry) = self.calls.get_mut(idx) else {
            return;
        };
        entry.record.status = if is_error {
            "error".to_owned()
        } else {
            "ok".to_owned()
        };
        let dur_ms = entry
            .started_at
            .elapsed()
            .as_secs_f64()
            .mul_add(1000.0, 0.0)
            .round() as u64;
        entry.record.duration_ms = Some(dur_ms);

        if is_error && !error_text.is_empty() {
            let truncated: String = error_text.chars().take(NESTED_MAX_ERROR_CHARS).collect();
            entry.record.error = Some(truncated);
        }
    }

    pub fn add_usage(&mut self, usage: &Usage) {
        self.usage = Some(match &self.usage {
            Some(prev) => combine_usage(prev, usage),
            None => usage.clone(),
        });
    }

    pub fn total_usage(&self) -> Option<Usage> {
        self.usage.clone()
    }

    pub fn snapshot(&self) -> Option<NestedToolCalls> {
        if self.calls.is_empty() && self.complete {
            return None;
        }
        let calls: Vec<NestedToolCallRecord> =
            self.calls.iter().map(|e| e.record.clone()).collect();
        let all_finished = calls.iter().all(|c| c.status != "unfinished");
        Some(NestedToolCalls {
            calls,
            complete: self.complete && all_finished,
        })
    }
}

#[derive(Clone, Default)]
pub struct NestedToolCallOptions {
    pub cancel_token: Option<CancellationToken>,
    pub on_update: Option<AgentToolUpdateCallback>,
}

pub trait NestedToolCallHost: Send + Sync {
    fn get_tools(&self) -> Vec<AgentTool>;
    fn is_sequential(&self) -> bool;
    fn run_tool_call(
        &self,
        tool_call: ToolCall,
        parent_tool_call_id: String,
        cancel_token: Option<CancellationToken>,
        on_update: ToolUpdateSink,
    ) -> BoxFuture<'static, AgentToolCallOutcome>;
    fn emit_event(&self, event: AgentEvent) -> BoxFuture<'static, ()>;
}

struct CallScope {
    recorder: Arc<Mutex<NestedCallRecorder>>,
    next_id: usize,
    holds_queue: bool,
}

fn text_of_result(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|b| match b {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 嵌套工具调用调度器（对照 `NestedToolCallRunner`）。
pub struct NestedToolCallRunner {
    host: Arc<dyn NestedToolCallHost>,
    scopes: Mutex<BTreeMap<String, CallScope>>,
    queue_tail: Mutex<Option<oneshot::Receiver<()>>>,
}

impl NestedToolCallRunner {
    pub fn new(host: Arc<dyn NestedToolCallHost>) -> Self {
        Self {
            host,
            scopes: Mutex::new(BTreeMap::new()),
            queue_tail: Mutex::new(None),
        }
    }

    pub async fn execute(
        &self,
        caller_id: &str,
        name: &str,
        args: JsonValue,
        options: NestedToolCallOptions,
    ) -> AgentToolCallOutcome {
        let (recorder, call_num, caller_holds_queue) = {
            let mut scopes = self.scopes.lock().expect("nested scopes lock poisoned");
            let scope = scopes
                .entry(caller_id.to_owned())
                .or_insert_with(|| CallScope {
                    recorder: Arc::new(Mutex::new(NestedCallRecorder::new())),
                    next_id: 1,
                    holds_queue: false,
                });
            let n = scope.next_id;
            scope.next_id += 1;
            (scope.recorder.clone(), n, scope.holds_queue)
        };

        let nested_id = format!("{caller_id}/{call_num}");
        let arg_map = args.as_object().cloned().unwrap_or_else(Map::new);
        let tool_call = ToolCall {
            id: nested_id.clone(),
            name: name.to_owned(),
            arguments: arg_map.clone(),
            thought_signature: None,
            namespace: None,
            arguments_error: None,
        };

        let record_idx = {
            let mut rec = recorder.lock().expect("recorder lock poisoned");
            rec.start(&tool_call)
        };

        self.host
            .emit_event(AgentEvent::ToolExecutionStart {
                tool_call_id: nested_id.clone(),
                tool_name: name.to_owned(),
                args: JsonValue::Object(arg_map.clone()),
                parent_tool_call_id: Some(caller_id.to_owned()),
            })
            .await;

        let tool_is_sequential = self
            .host
            .get_tools()
            .iter()
            .find(|t| t.name == name)
            .is_some_and(|t| t.execution_mode == Some(ToolExecutionMode::Sequential));
        let exclusive = !caller_holds_queue && (self.host.is_sequential() || tool_is_sequential);

        let mut release_tx: Option<oneshot::Sender<()>> = None;
        if exclusive {
            let prev_rx = {
                let (tx, rx) = oneshot::channel();
                release_tx = Some(tx);
                let mut tail_guard = self.queue_tail.lock().expect("queue_tail lock poisoned");
                tail_guard.replace(rx)
            };
            if let Some(prev) = prev_rx {
                let _ = prev.await;
            }
        }

        {
            let mut scopes = self.scopes.lock().expect("nested scopes lock poisoned");
            scopes.insert(
                nested_id.clone(),
                CallScope {
                    recorder: recorder.clone(),
                    next_id: 1,
                    holds_queue: caller_holds_queue || exclusive,
                },
            );
        }

        let host_for_update = self.host.clone();
        let nested_id_for_update = nested_id.clone();
        let name_for_update = name.to_owned();
        let args_for_update = JsonValue::Object(arg_map);
        let caller_for_update = caller_id.to_owned();
        let user_on_update = options.on_update.clone();

        let update_sink: ToolUpdateSink = Arc::new(move |partial_result| {
            if let Some(cb) = &user_on_update {
                cb(partial_result.clone());
            }
            let host = host_for_update.clone();
            let tc_id = nested_id_for_update.clone();
            let t_name = name_for_update.clone();
            let t_args = args_for_update.clone();
            let parent_id = caller_for_update.clone();
            Box::pin(async move {
                host.emit_event(AgentEvent::ToolExecutionUpdate {
                    tool_call_id: tc_id,
                    tool_name: t_name,
                    args: t_args,
                    partial_result,
                    parent_tool_call_id: Some(parent_id),
                })
                .await;
            })
        });

        let outcome = self
            .host
            .run_tool_call(
                tool_call,
                caller_id.to_owned(),
                options.cancel_token,
                update_sink,
            )
            .await;

        {
            let mut scopes = self.scopes.lock().expect("nested scopes lock poisoned");
            scopes.remove(&nested_id);
        }
        if let Some(tx) = release_tx {
            let _ = tx.send(());
        }

        {
            let mut rec = recorder.lock().expect("recorder lock poisoned");
            rec.finish(
                record_idx,
                outcome.is_error,
                &text_of_result(&outcome.result),
            );
            if let Some(u) = &outcome.result.usage {
                rec.add_usage(u);
            }
        }

        self.host
            .emit_event(AgentEvent::ToolExecutionEnd {
                tool_call_id: nested_id,
                tool_name: name.to_owned(),
                result: outcome.result.clone(),
                is_error: outcome.is_error,
                duration_ms: outcome.duration_ms,
                parent_tool_call_id: Some(caller_id.to_owned()),
            })
            .await;

        outcome
    }

    pub fn take_record(&self, tool_call_id: &str) -> Option<NestedCallSummary> {
        let scope = {
            let mut scopes = self.scopes.lock().expect("nested scopes lock poisoned");
            scopes.remove(tool_call_id)?
        };
        let rec = scope.recorder.lock().expect("recorder lock poisoned");
        Some(NestedCallSummary {
            calls: rec.snapshot(),
            usage: rec.total_usage(),
        })
    }

    pub fn clear(&self) {
        if let Ok(mut scopes) = self.scopes.lock() {
            scopes.clear();
        }
    }
}

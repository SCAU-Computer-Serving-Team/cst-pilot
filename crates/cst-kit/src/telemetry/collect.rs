use std::collections::BTreeMap;

use pi_agent::AgentToolResult;
use pi_ai::{AssistantMessage, StopReason, UserContentBlock};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{billing::Billing, config::Config, environment::Environment, record::SessionRecord};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelAgg {
    pub provider: String,
    pub model: String,
    pub thinking_level: String,
    pub turns: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total_tokens: u64,
    pub cost: Option<f64>,
    pub currency: String,
    pub incomplete_usage_turns: u64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAgg {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub calls: u64,
    pub failures: u64,
    pub degraded: u64,
    pub total_ms: u64,
    pub max_ms: u64,
    pub result_bytes: u64,
    pub truncated: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entries_total: Option<u64>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub providers: BTreeMap<String, u64>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub modes: BTreeMap<String, u64>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metrics {
    pub prompts: u64,
    pub turns: u64,
    pub active_ms: u64,
    pub models: Vec<ModelAgg>,
    pub tools: Vec<ToolAgg>,
    pub compactions: u64,
    pub compaction_tokens: u64,
    pub compaction_overflows: u64,
    pub compaction_failures: u64,
    pub context_peak: u64,
    pub context_window: u64,
    pub provider_errors: BTreeMap<String, u64>,
    pub network_errors: u64,
    pub aborted: u64,
    pub tool_failures: u64,
    pub errors: Vec<ErrorGroup>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErrorGroup {
    pub message: String,
    pub count: u64,
}
struct PendingTool {
    name: String,
    scope: Option<String>,
    at: u64,
}
/// Event handlers retain only aggregate fields, timing and local billing IDs, never prompt/tool bodies.
pub struct Collector {
    pub record_id: String,
    pub session_id: String,
    pub channel: String,
    pub reason: String,
    pub started_at: u64,
    pub last_activity: u64,
    pub billing: Billing,
    environment: Environment,
    kit_version: String,
    metrics: Metrics,
    currency: Option<BTreeMap<String, String>>,
    models: BTreeMap<(String, String, String), ModelAgg>,
    tools: BTreeMap<(String, Option<String>), ToolAgg>,
    errors: BTreeMap<String, u64>,
    turn_start: Option<u64>,
    thinking: String,
    saw_response: bool,
    billing_attempt: Option<usize>,
    pending: BTreeMap<String, PendingTool>,
}
impl Collector {
    pub fn new(
        session_id: String,
        reason: String,
        channel: String,
        at: u64,
        environment: Environment,
        kit_version: String,
        config: &Config,
    ) -> Self {
        Self {
            record_id: uuid::Uuid::new_v4().to_string(),
            session_id,
            reason,
            channel,
            started_at: at,
            last_activity: at,
            environment,
            kit_version,
            metrics: Metrics::default(),
            billing: Billing::default(),
            currency: config.currency.clone(),
            models: BTreeMap::new(),
            tools: BTreeMap::new(),
            errors: BTreeMap::new(),
            turn_start: None,
            thinking: String::new(),
            saw_response: false,
            billing_attempt: None,
            pending: BTreeMap::new(),
        }
    }
    pub fn input(&mut self, source: &str, at: u64) {
        if matches!(source, "interactive" | "rpc") {
            add(&mut self.metrics.prompts, 1);
            self.activity(at);
        }
    }
    pub fn activity(&mut self, at: u64) {
        self.last_activity = self.last_activity.max(at);
    }
    pub fn turn_start(&mut self, at: u64) {
        self.turn_start = Some(at);
        self.activity(at);
    }
    /// Invoked for every HTTP attempt via before_provider_headers, including internal retries.
    pub fn before_request(&mut self, provider: &str, thinking: &str, at: u64) {
        self.thinking = thinking.into();
        self.saw_response = false;
        self.billing_attempt = if provider == "cstoa" {
            Some(self.billing.start())
        } else {
            None
        };
        self.activity(at);
    }
    pub fn after_response(&mut self, status: u16, headers: &BTreeMap<String, String>, at: u64) {
        self.saw_response = true;
        if !(200..300).contains(&status) {
            add(
                self.metrics
                    .provider_errors
                    .entry(status.to_string())
                    .or_default(),
                1,
            );
        }
        if let Some(attempt) = self.billing_attempt.take() {
            let id = headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("x-oneapi-request-id"))
                .map(|(_, value)| value.as_str());
            self.billing.finish(attempt, id);
        }
        self.activity(at);
    }
    pub fn turn_end(
        &mut self,
        message: Option<&AssistantMessage>,
        context: Option<(u64, u64)>,
        at: u64,
    ) {
        if let Some(start) = self.turn_start.take() {
            add(&mut self.metrics.active_ms, at.saturating_sub(start));
        }
        add(&mut self.metrics.turns, 1);
        self.activity(at);
        if let Some(message) = message {
            let thinking = self.thinking.clone();
            let key = (
                message.provider.clone(),
                message.model.clone(),
                thinking.clone(),
            );
            if self.models.contains_key(&key) || self.models.len() < 32 {
                let agg = self.models.entry(key).or_insert_with(|| ModelAgg {
                    provider: message.provider.clone(),
                    model: message.model.clone(),
                    thinking_level: thinking,
                    cost: if message.provider == "cstoa" {
                        None
                    } else {
                        Some(0.0)
                    },
                    currency: self
                        .currency
                        .as_ref()
                        .map(|map| {
                            map.get(&message.provider)
                                .map(String::as_str)
                                .unwrap_or("USD")
                        })
                        .unwrap_or("")
                        .into(),
                    ..Default::default()
                });
                add(&mut agg.turns, 1);
                if message.usage.total_tokens == 0
                    || matches!(message.stop_reason, StopReason::Aborted | StopReason::Error)
                {
                    add(&mut agg.incomplete_usage_turns, 1);
                }
                let usage = &message.usage;
                add(&mut agg.input, usage.input);
                add(&mut agg.output, usage.output);
                add(&mut agg.cache_read, usage.cache_read);
                add(&mut agg.cache_write, usage.cache_write);
                add(&mut agg.total_tokens, usage.total_tokens);
                agg.cost = agg.cost.and_then(|cost| {
                    let total = cost + usage.cost.total;
                    (usage.cost.total.is_finite() && usage.cost.total >= 0.0 && total.is_finite())
                        .then_some(total)
                });
            }
            if message.stop_reason == StopReason::Aborted {
                add(&mut self.metrics.aborted, 1);
            }
            if message.stop_reason == StopReason::Error {
                if !self.saw_response {
                    add(&mut self.metrics.network_errors, 1);
                }
                if let Some(text) = message
                    .error_message
                    .as_ref()
                    .filter(|message| !message.is_empty())
                {
                    add(self.errors.entry(text.clone()).or_default(), 1);
                }
            }
        }
        if let Some((tokens, window)) =
            context.filter(|(tokens, _)| *tokens > self.metrics.context_peak)
        {
            self.metrics.context_peak = tokens;
            self.metrics.context_window = window;
        }
        self.thinking.clear();
        self.billing_attempt = None;
    }
    pub fn compact(&mut self, tokens: u64, overflow: bool) {
        add(&mut self.metrics.compactions, 1);
        add(&mut self.metrics.compaction_tokens, tokens);
        if overflow {
            add(&mut self.metrics.compaction_overflows, 1);
        }
    }
    pub fn compact_failed(&mut self) {
        add(&mut self.metrics.compaction_failures, 1);
    }
    pub fn tool_start(&mut self, id: &str, name: &str, input: &Value, at: u64) {
        if self.pending.contains_key(id) || self.pending.len() >= 1024 {
            return;
        }
        let default = match name {
            "sys" => Some("overview"),
            "driver" => Some("problem"),
            "eventlog" => Some("recent"),
            "bitlocker" => Some("status"),
            _ => None,
        };
        let scope = if default.is_some() || name == "disk" {
            input
                .get("scope")
                .and_then(Value::as_str)
                .filter(|scope| match name {
                    "sys" => matches!(*scope, "overview" | "proc" | "gpu" | "io" | "sensor"),
                    "driver" => matches!(*scope, "problem" | "core" | "external" | "find"),
                    "eventlog" => matches!(
                        *scope,
                        "recent"
                            | "boot"
                            | "crash"
                            | "service"
                            | "disk"
                            | "security"
                            | "find"
                            | "detail"
                    ),
                    "bitlocker" => matches!(*scope, "status" | "unlock" | "decrypt"),
                    "disk" => matches!(*scope, "space" | "info" | "health" | "all" | "usage"),
                    _ => false,
                })
                .or(default)
                .map(str::to_owned)
        } else {
            None
        };
        self.pending.insert(
            id.into(),
            PendingTool {
                name: name.into(),
                scope,
                at,
            },
        );
        self.activity(at);
    }
    pub fn tool_end(
        &mut self,
        id: &str,
        result: &AgentToolResult,
        is_error: bool,
        duration: Option<u64>,
        at: u64,
    ) {
        let Some(start) = self.pending.remove(id) else {
            return;
        };
        self.activity(at);
        if is_error {
            add(&mut self.metrics.tool_failures, 1);
        }
        let key = (start.name.clone(), start.scope.clone());
        if !self.tools.contains_key(&key) && self.tools.len() >= 512 {
            return;
        }
        let agg = self.tools.entry(key).or_insert_with(|| ToolAgg {
            name: start.name.clone(),
            scope: start.scope,
            ..Default::default()
        });
        add(&mut agg.calls, 1);
        if is_error {
            add(&mut agg.failures, 1);
        }
        let elapsed = duration.unwrap_or_else(|| at.saturating_sub(start.at));
        add(&mut agg.total_ms, elapsed);
        agg.max_ms = agg.max_ms.max(elapsed);
        let text: String = result
            .content
            .iter()
            .filter_map(|block| match block {
                UserContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect();
        add(&mut agg.result_bytes, text.len() as u64);
        let details = &result.details;
        let text_truncated = serde_json::from_str::<Value>(&text)
            .ok()
            .is_some_and(|v| v.get("outputTruncated") == Some(&Value::Bool(true)));
        if text_truncated
            || details.pointer("/truncation/truncated") == Some(&Value::Bool(true))
            || details.get("outputTruncated") == Some(&Value::Bool(true))
        {
            add(&mut agg.truncated, 1);
        }
        let scope = agg.scope.as_deref().unwrap_or("");
        if details.get("degraded") == Some(&Value::Bool(true))
            || details.get(&agg.name).and_then(|v| v.get("degraded")) == Some(&Value::Bool(true))
            || details.get(scope).and_then(|v| v.get("degraded")) == Some(&Value::Bool(true))
            || (agg.name == "disk"
                && (details.pointer("/usage/method").and_then(Value::as_str) == Some("node-walk")
                    || details
                        .pointer("/usage/degradedFrom")
                        .and_then(Value::as_str)
                        .is_some_and(|v| !v.is_empty())))
        {
            add(&mut agg.degraded, 1);
        }
        if !is_error && matches!(agg.name.as_str(), "runbook" | "runbook_deliver") {
            let count = details
                .get("items")
                .or_else(|| details.pointer("/runbook/items"))
                .and_then(|v| v.as_u64().or_else(|| v.as_array().map(|a| a.len() as u64)))
                .unwrap_or(0);
            add(agg.entries_total.get_or_insert(0), count);
        }
        // Distributions are from public enum-like output fields only; never record URL/provider headers.
        if agg.name == "web_search" {
            distribution(&mut agg.providers, details.get("provider"));
        }
        if agg.name == "fetch_content" {
            distribution(&mut agg.modes, details.get("mode"));
        }
    }
    pub fn has_activity(&self) -> bool {
        self.metrics.prompts > 0 || self.metrics.turns > 0 || self.billing.summary().is_some()
    }
    pub fn snapshot(
        &self,
        revision: u64,
        final_state: bool,
        end_reason: &str,
        ended_at: u64,
        context_entries: Option<u64>,
    ) -> SessionRecord {
        let mut metrics = self.metrics.clone();
        metrics.models = self.models.values().cloned().collect();
        metrics.tools = self.tools.values().cloned().collect();
        let mut errors: Vec<_> = self
            .errors
            .iter()
            .map(|(message, count)| ErrorGroup {
                message: message.clone(),
                count: *count,
            })
            .collect();
        errors.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.message.cmp(&b.message))
        });
        errors.truncate(10);
        metrics.errors = errors;
        SessionRecord {
            v: "0.2".into(),
            kind: "session".into(),
            record_id: self.record_id.clone(),
            revision,
            final_state,
            kit_version: self.kit_version.clone(),
            session_id: self.session_id.clone(),
            channel: self.channel.clone(),
            reason: self.reason.clone(),
            end_reason: end_reason.into(),
            started_at: super::record::local_iso(self.started_at),
            ended_at: super::record::local_iso(ended_at),
            duration_ms: ended_at.saturating_sub(self.started_at),
            context_entries,
            environment: self.environment.clone(),
            metrics,
            cstoa_billing: self.billing.summary(),
        }
    }
}
fn add(target: &mut u64, value: u64) {
    *target = target.saturating_add(value).min(9_007_199_254_740_991);
}
fn distribution(map: &mut BTreeMap<String, u64>, value: Option<&Value>) {
    if let Some(value) = value.and_then(Value::as_str).filter(|v| {
        !v.is_empty()
            && v.len() <= 64
            && v.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    }) && (map.contains_key(value) || map.len() < 16)
    {
        add(map.entry(value.into()).or_default(), 1);
    }
}

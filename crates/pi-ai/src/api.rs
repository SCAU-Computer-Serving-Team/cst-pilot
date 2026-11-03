pub mod anthropic_messages;
pub mod google_generative_ai;
pub mod openai_completions;
pub mod openai_responses;
pub mod typesafe_system_one;

use std::collections::BTreeMap;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::types::{
    CacheRetention, ClientIdentity, Message, Model, ModelCost, ModelThinkingLevel, ProviderEnv,
    ProviderHeaders, SamplingParams, SimpleStreamOptions, StreamOptions, ThinkingBudgets,
    ThinkingLevel, TranscriptContext, Usage, UserContent, UserContentBlock,
};

pub const PI_AI_VERSION: &str = "1.5.0";
pub const DEFAULT_MAX_RETRY_DELAY_MS: u64 = 60_000;
pub const MIN_ANSWER_TOKENS: u64 = 1024;

// =============================================================================
// Cost & Token Helpers (对照 models.ts / simple-options.ts)
// =============================================================================

pub fn calculate_cost(cost: &ModelCost, usage: &mut Usage) {
    let total_input = usage.input + usage.cache_read + usage.cache_write;
    let mut rates_input = cost.input;
    let mut rates_output = cost.output;
    let mut rates_cache_read = cost.cache_read;
    let mut rates_cache_write = cost.cache_write;

    if let Some(tiers) = &cost.tiers {
        let mut best_threshold = 0;
        for tier in tiers {
            if total_input > tier.input_tokens_above && tier.input_tokens_above >= best_threshold {
                best_threshold = tier.input_tokens_above;
                rates_input = tier.input;
                rates_output = tier.output;
                rates_cache_read = tier.cache_read;
                rates_cache_write = tier.cache_write;
            }
        }
    }

    let long_write = usage.cache_write_1h.unwrap_or(0).min(usage.cache_write);
    let short_write = usage.cache_write - long_write;
    usage.cost.input = (usage.input as f64 / 1_000_000.0) * rates_input;
    usage.cost.output = (usage.output as f64 / 1_000_000.0) * rates_output;
    usage.cost.cache_read = (usage.cache_read as f64 / 1_000_000.0) * rates_cache_read;
    usage.cost.cache_write = (rates_cache_write * short_write as f64
        + rates_input * 2.0 * long_write as f64)
        / 1_000_000.0;
    if usage.cost.total_from_provider != Some(true) {
        usage.cost.total =
            usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
    }
}

pub fn is_thinking_level_supported(model: &Model, level: ModelThinkingLevel) -> bool {
    if level == ModelThinkingLevel::Off {
        return model
            .thinking_level_map
            .as_ref()
            .and_then(|m| m.get(&ModelThinkingLevel::Off))
            != Some(&None);
    }
    if !model.reasoning {
        return false;
    }
    if let Some(map) = &model.thinking_level_map
        && let Some(mapped) = map.get(&level)
    {
        return mapped.is_some();
    }
    !matches!(level, ModelThinkingLevel::XHigh | ModelThinkingLevel::Max)
}

pub fn clamp_thinking_level(model: &Model, level: ModelThinkingLevel) -> ModelThinkingLevel {
    if !model.reasoning {
        return ModelThinkingLevel::Off;
    }
    let order = [
        ModelThinkingLevel::Off,
        ModelThinkingLevel::Minimal,
        ModelThinkingLevel::Low,
        ModelThinkingLevel::Medium,
        ModelThinkingLevel::High,
        ModelThinkingLevel::XHigh,
        ModelThinkingLevel::Max,
    ];
    let target_idx = order.iter().position(|&l| l == level).unwrap_or(0);
    // 先向下寻找不超过目标档位的最近可用档位
    for idx in (0..=target_idx).rev() {
        if is_thinking_level_supported(model, order[idx]) {
            return order[idx];
        }
    }
    // 若向下均不支持（例如 off 被禁用），则向上寻找最低可用档位
    for &candidate in &order[target_idx + 1..] {
        if is_thinking_level_supported(model, candidate) {
            return candidate;
        }
    }
    ModelThinkingLevel::Off
}

pub fn resolve_sampling_params(
    model: &Model,
    thinking_level: ModelThinkingLevel,
    request_params: Option<&SamplingParams>,
) -> Option<SamplingParams> {
    let effective = clamp_thinking_level(model, thinking_level);
    let level_params = model
        .sampling_params_by_thinking_level
        .as_ref()
        .and_then(|m| m.get(&effective));

    if model.sampling_params.is_none() && level_params.is_none() && request_params.is_none() {
        return None;
    }

    let mut merged = SamplingParams::new();
    if let Some(base) = &model.sampling_params {
        merged.extend(base.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    if let Some(lp) = level_params {
        merged.extend(lp.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    if let Some(rp) = request_params {
        merged.extend(rp.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    Some(merged)
}

pub fn build_base_options(
    model: &Model,
    _context: &TranscriptContext,
    options: Option<&SimpleStreamOptions>,
) -> StreamOptions {
    let Some(opts) = options else {
        return StreamOptions {
            max_tokens: Some(model.max_tokens),
            sampling_params: resolve_sampling_params(model, ModelThinkingLevel::Off, None),
            ..StreamOptions::default()
        };
    };

    let mut base = opts.base.clone();
    let thinking_level = opts
        .reasoning
        .map(ModelThinkingLevel::from)
        .unwrap_or(ModelThinkingLevel::Off);
    base.sampling_params =
        resolve_sampling_params(model, thinking_level, opts.base.sampling_params.as_ref());
    if base.max_tokens.is_none() {
        base.max_tokens = Some(model.max_tokens);
    }
    if opts.tool_choice.is_some() {
        base.tool_choice = opts.tool_choice;
    }
    if opts.thinking_budgets.is_some() {
        base.thinking_budgets = opts.thinking_budgets.clone();
    }
    if opts.reasoning.is_some() {
        base.reasoning_effort = opts.reasoning;
    }
    base
}

pub fn thinking_budget_for_level(
    level: ThinkingLevel,
    custom_budgets: Option<&ThinkingBudgets>,
) -> u64 {
    let default_minimal = custom_budgets.and_then(|b| b.minimal).unwrap_or(1024) as u64;
    let default_low = custom_budgets.and_then(|b| b.low).unwrap_or(2048) as u64;
    let default_medium = custom_budgets.and_then(|b| b.medium).unwrap_or(8192) as u64;
    let default_high = custom_budgets.and_then(|b| b.high).unwrap_or(16384) as u64;

    match level {
        ThinkingLevel::Minimal => default_minimal,
        ThinkingLevel::Low => default_low,
        ThinkingLevel::Medium => default_medium,
        ThinkingLevel::High | ThinkingLevel::XHigh | ThinkingLevel::Max => default_high,
    }
}

pub fn adjust_max_tokens_for_thinking(
    base_max_tokens: Option<u64>,
    model_max_tokens: u64,
    reasoning_level: ThinkingLevel,
    custom_budgets: Option<&ThinkingBudgets>,
) -> (u64, u64) {
    let mut thinking_budget = thinking_budget_for_level(reasoning_level, custom_budgets);
    let max_tokens = match base_max_tokens {
        None => model_max_tokens,
        Some(base) => (base + thinking_budget).min(model_max_tokens),
    };
    if max_tokens <= thinking_budget {
        thinking_budget = thinking_budget.min(max_tokens.saturating_sub(MIN_ANSWER_TOKENS));
    }
    (max_tokens, thinking_budget)
}

pub fn resolve_cache_retention(
    retention: Option<CacheRetention>,
    env: Option<&ProviderEnv>,
) -> CacheRetention {
    if let Some(r) = retention {
        return r;
    }
    if get_provider_env("PI_CACHE_RETENTION", env).as_deref() == Some("long") {
        return CacheRetention::Long;
    }
    CacheRetention::Short
}

pub fn get_provider_env(name: &str, env: Option<&ProviderEnv>) -> Option<String> {
    if let Some(map) = env
        && let Some(val) = map.get(name)
        && !val.is_empty()
    {
        return Some(val.clone());
    }
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

pub fn short_hash(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut hex = String::with_capacity(16);
    for b in &digest[..8] {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

pub fn clamp_openai_prompt_cache_key(session_id: Option<&str>) -> Option<String> {
    let sid = session_id?.trim();
    if sid.is_empty() {
        return None;
    }
    if sid.len() <= 64 {
        Some(sid.to_owned())
    } else {
        Some(format!("pi_{}", short_hash(sid)))
    }
}

// =============================================================================
// Headers & Client Identity
// =============================================================================

pub fn has_header(headers: Option<&ProviderHeaders>, name: &str) -> bool {
    let Some(map) = headers else {
        return false;
    };
    map.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case(name) && v.as_ref().is_some_and(|s| !s.trim().is_empty())
    })
}

pub fn infer_copilot_initiator(messages: &[Message]) -> &'static str {
    match messages.last() {
        Some(msg) if !matches!(msg, Message::User(_)) => "agent",
        _ => "user",
    }
}

pub fn has_copilot_vision_input(messages: &[Message]) -> bool {
    messages.iter().any(|msg| match msg {
        Message::User(u) => match &u.content {
            UserContent::Text(_) => false,
            UserContent::Blocks(blocks) => blocks
                .iter()
                .any(|b| matches!(b, UserContentBlock::Image(_))),
        },
        Message::ToolResult(tr) => tr
            .content
            .iter()
            .any(|b| matches!(b, UserContentBlock::Image(_))),
        _ => false,
    })
}

pub fn inject_copilot_dynamic_headers(
    model: &Model,
    messages: &[Message],
    headers: &mut ProviderHeaders,
) {
    if model.provider != "github-copilot" {
        return;
    }
    if !headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("x-initiator"))
    {
        headers.insert(
            "X-Initiator".to_owned(),
            Some(infer_copilot_initiator(messages).to_owned()),
        );
    }
    if !headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("openai-intent"))
    {
        headers.insert(
            "Openai-Intent".to_owned(),
            Some("conversation-edits".to_owned()),
        );
    }
    if has_copilot_vision_input(messages)
        && !headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("copilot-vision-request"))
    {
        headers.insert("Copilot-Vision-Request".to_owned(), Some("true".to_owned()));
    }
}

pub fn require_client_api_key(
    provider: &str,
    api_key: Option<&str>,
    headers: Option<&ProviderHeaders>,
) -> Result<String, String> {
    if let Some(key) = api_key
        && !key.is_empty()
    {
        return Ok(key.to_owned());
    }
    if has_header(headers, "authorization") || has_header(headers, "cf-aig-authorization") {
        return Ok("unused".to_owned());
    }
    Err(format!("No API key for provider: {provider}"))
}

pub fn build_header_map(
    identity: Option<&ClientIdentity>,
    default_headers: &[(&str, &str)],
    model_headers: Option<&BTreeMap<String, String>>,
    option_headers: Option<&ProviderHeaders>,
) -> HeaderMap {
    let mut merged: BTreeMap<String, (String, String)> = BTreeMap::new();

    let ua = identity
        .and_then(|id| id.user_agent.clone())
        .unwrap_or_else(|| {
            format!(
                "pi/{PI_AI_VERSION} ({}; rust; {})",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        });
    merged.insert("user-agent".to_owned(), ("User-Agent".to_owned(), ua));

    if let Some(id) = identity {
        if let Some(app_name) = &id.app_name {
            merged.insert(
                "x-openrouter-title".to_owned(),
                ("X-OpenRouter-Title".to_owned(), app_name.clone()),
            );
        }
        if let Some(app_url) = &id.app_url {
            merged.insert(
                "http-referer".to_owned(),
                ("HTTP-Referer".to_owned(), app_url.clone()),
            );
        }
    }

    for &(k, v) in default_headers {
        merged.insert(k.to_ascii_lowercase(), (k.to_owned(), v.to_owned()));
    }

    if let Some(mh) = model_headers {
        for (k, v) in mh {
            merged.insert(k.to_ascii_lowercase(), (k.clone(), v.clone()));
        }
    }

    if let Some(oh) = option_headers {
        for (k, v) in oh {
            let lower = k.to_ascii_lowercase();
            match v {
                Some(val) => {
                    merged.insert(lower, (k.clone(), val.clone()));
                }
                None => {
                    merged.remove(&lower);
                }
            }
        }
    }

    let mut header_map = HeaderMap::new();
    for (_, (orig_name, val)) in merged {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(orig_name.as_bytes()),
            HeaderValue::from_str(&val),
        ) {
            header_map.insert(name, value);
        }
    }
    header_map
}

// =============================================================================
// SSE Incremental Parser
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SseParser {
    incomplete_utf8: Vec<u8>,
    buffer: String,
    current_event: Option<String>,
    current_data: Vec<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入原始网络字节块，自动保留跨 chunk 截断的 UTF-8 多字节字符前缀。
    pub fn feed_bytes(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        if bytes.is_empty() {
            return Vec::new();
        }
        self.incomplete_utf8.extend_from_slice(bytes);
        loop {
            match std::str::from_utf8(&self.incomplete_utf8) {
                Ok(valid) => {
                    self.buffer.push_str(valid);
                    self.incomplete_utf8.clear();
                    break;
                }
                Err(err) => {
                    let valid_up_to = err.valid_up_to();
                    if valid_up_to > 0 {
                        let valid_prefix =
                            std::str::from_utf8(&self.incomplete_utf8[..valid_up_to])
                                .expect("valid_up_to guarantees valid UTF-8");
                        self.buffer.push_str(valid_prefix);
                    }
                    match err.error_len() {
                        Some(invalid_len) => {
                            self.buffer.push('\u{FFFD}');
                            self.incomplete_utf8.drain(..valid_up_to + invalid_len);
                        }
                        None => {
                            self.incomplete_utf8.drain(..valid_up_to);
                            break;
                        }
                    }
                }
            }
        }
        self.drain_lines()
    }

    pub fn feed(&mut self, chunk: &str) -> Vec<SseEvent> {
        self.buffer.push_str(chunk);
        self.drain_lines()
    }

    fn drain_lines(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();

        while let Some(newline_pos) = self.buffer.find('\n') {
            let mut line = self.buffer[..newline_pos].to_owned();
            self.buffer.drain(..=newline_pos);
            if line.ends_with('\r') {
                line.pop();
            }
            self.process_line(&line, &mut events);
        }

        events
    }

    pub fn finish(&mut self) -> Vec<SseEvent> {
        if !self.incomplete_utf8.is_empty() {
            self.buffer.push('\u{FFFD}');
            self.incomplete_utf8.clear();
        }
        let mut events = Vec::new();
        if !self.buffer.is_empty() {
            let mut line = std::mem::take(&mut self.buffer);
            if line.ends_with('\r') {
                line.pop();
            }
            self.process_line(&line, &mut events);
        }
        self.flush_event(&mut events);
        events
    }

    fn process_line(&mut self, line: &str, events: &mut Vec<SseEvent>) {
        if line.is_empty() {
            self.flush_event(events);
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, raw_value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => {
                self.current_event = Some(raw_value.to_owned());
            }
            "data" => {
                self.current_data.push(raw_value.to_owned());
            }
            _ => {}
        }
    }

    fn flush_event(&mut self, events: &mut Vec<SseEvent>) {
        if self.current_data.is_empty() && self.current_event.is_none() {
            return;
        }
        let data = self.current_data.join("\n");
        self.current_data.clear();
        let event = self.current_event.take();
        if !data.is_empty() || event.is_some() {
            events.push(SseEvent { event, data });
        }
    }
}

// =============================================================================
// HTTP Retry & Error Formatting
// =============================================================================

pub fn is_retryable_status(status: u16, should_retry_header: Option<&str>) -> bool {
    if should_retry_header == Some("true") {
        return true;
    }
    if should_retry_header == Some("false") {
        return false;
    }
    status == 408 || status == 409 || status == 429 || status >= 500
}

pub fn extract_error_message_from_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "Empty error response".to_owned();
    }
    if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
        if let Some(msg) = val
            .get("error")
            .and_then(|e| e.get("message").or(Some(e)))
            .and_then(|m| m.as_str())
        {
            return msg.to_owned();
        }
        if let Some(msg) = val.get("message").and_then(|m| m.as_str()) {
            return msg.to_owned();
        }
    }
    trimmed.chars().take(500).collect()
}

pub async fn read_response_text_cancellable(
    resp: reqwest::Response,
    cancel_token: Option<&CancellationToken>,
) -> Result<String, String> {
    let text_fut = resp.text();
    let res = if let Some(token) = cancel_token {
        tokio::select! {
            _ = token.cancelled() => return Err("Request was aborted".to_owned()),
            r = text_fut => r,
        }
    } else {
        text_fut.await
    };
    res.map_err(|e| {
        if cancel_token.is_some_and(|t| t.is_cancelled()) {
            "Request was aborted".to_owned()
        } else {
            format!("Response body read error: {e}")
        }
    })
}

pub async fn read_response_json_cancellable(
    resp: reqwest::Response,
    cancel_token: Option<&CancellationToken>,
) -> Result<Value, String> {
    let text = read_response_text_cancellable(resp, cancel_token).await?;
    serde_json::from_str::<Value>(&text).map_err(|e| format!("Invalid JSON response: {e}"))
}

#[allow(clippy::too_many_arguments)]
pub async fn send_with_retry(
    client: &reqwest::Client,
    url: &str,
    headers: HeaderMap,
    payload: &Value,
    timeout_ms: Option<u64>,
    max_retries: u32,
    max_retry_delay_ms: Option<u64>,
    cancel_token: Option<&CancellationToken>,
) -> Result<reqwest::Response, String> {
    let mut retries_remaining = max_retries;
    let max_delay = max_retry_delay_ms.unwrap_or(DEFAULT_MAX_RETRY_DELAY_MS);

    loop {
        if cancel_token.is_some_and(|t| t.is_cancelled()) {
            return Err("Request was aborted".to_owned());
        }

        let mut req = client.post(url).headers(headers.clone()).json(payload);
        if let Some(ms) = timeout_ms {
            req = req.timeout(Duration::from_millis(ms));
        }

        let send_fut = req.send();
        let response_res = if let Some(token) = cancel_token {
            tokio::select! {
                _ = token.cancelled() => return Err("Request was aborted".to_owned()),
                res = send_fut => res,
            }
        } else {
            send_fut.await
        };

        match response_res {
            Ok(resp) => {
                let status = resp.status().as_u16();
                if resp.status().is_success() {
                    return Ok(resp);
                }

                let should_retry = resp
                    .headers()
                    .get("x-should-retry")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_owned());
                let retry_after_ms = resp
                    .headers()
                    .get("retry-after-ms")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<f64>().ok());
                let retry_after_sec = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<f64>().ok());

                let body_text = match read_response_text_cancellable(resp, cancel_token).await {
                    Ok(t) => t,
                    Err(err) if cancel_token.is_some_and(|t| t.is_cancelled()) => {
                        return Err(err);
                    }
                    Err(_) => String::new(),
                };
                let err_detail = extract_error_message_from_body(&body_text);
                let formatted_err = format!("HTTP {status}: {err_detail}");

                if retries_remaining == 0 || !is_retryable_status(status, should_retry.as_deref()) {
                    return Err(formatted_err);
                }

                let retry_index = max_retries - retries_remaining;
                retries_remaining -= 1;

                let delay_ms = if let Some(ms) = retry_after_ms {
                    ms.max(0.0) as u64
                } else if let Some(sec) = retry_after_sec {
                    (sec * 1000.0).max(0.0) as u64
                } else {
                    (500u64.saturating_mul(1u64 << retry_index.min(4))).min(8000)
                };

                if max_delay > 0 && delay_ms > max_delay {
                    return Err(format!(
                        "Server requested {}s retry delay (max: {}s). {}",
                        delay_ms.div_ceil(1000),
                        max_delay.div_ceil(1000),
                        formatted_err
                    ));
                }

                if let Some(token) = cancel_token {
                    tokio::select! {
                        _ = token.cancelled() => return Err("Request was aborted".to_owned()),
                        _ = tokio::time::sleep(Duration::from_millis(delay_ms)) => {}
                    }
                } else {
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                }
            }
            Err(err) => {
                if cancel_token.is_some_and(|t| t.is_cancelled()) {
                    return Err("Request was aborted".to_owned());
                }
                if retries_remaining == 0 {
                    return Err(format!("Request error: {err}"));
                }
                let retry_index = max_retries - retries_remaining;
                retries_remaining -= 1;
                let delay_ms = (500u64.saturating_mul(1u64 << retry_index.min(4))).min(8000);
                if let Some(token) = cancel_token {
                    tokio::select! {
                        _ = token.cancelled() => return Err("Request was aborted".to_owned()),
                        _ = tokio::time::sleep(Duration::from_millis(delay_ms)) => {}
                    }
                } else {
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                }
            }
        }
    }
}

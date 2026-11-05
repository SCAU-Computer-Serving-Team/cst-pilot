use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::types::{AssistantMessage, StopReason};

pub type RetryBoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub const DEFAULT_MAX_AGENT_RETRY_DELAY_MS: u64 = 60_000;

/// 瞬态错误指数退避重试策略（对照 `RetryPolicy`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryPolicy {
    pub enabled: bool,
    pub max_retries: u32,
    pub base_delay_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_agent_delay_ms: Option<u64>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 2_000,
            max_agent_delay_ms: Some(DEFAULT_MAX_AGENT_RETRY_DELAY_MS),
        }
    }
}

/// 计算第 `attempt` 次（从 1 开始）重试的退避延迟毫秒数（对照 `retryDelayMs`）。
pub fn retry_delay_ms(policy: &RetryPolicy, attempt: u32) -> u64 {
    let exp = attempt.saturating_sub(1).min(62);
    let factor = 1u64.checked_shl(exp).unwrap_or(u64::MAX);
    let delay = policy.base_delay_ms.saturating_mul(factor);
    let cap = policy
        .max_agent_delay_ms
        .unwrap_or(DEFAULT_MAX_AGENT_RETRY_DELAY_MS);
    delay.min(cap)
}

pub type OnRetryScheduledFn =
    Arc<dyn Fn(u32, u32, u64, String) -> RetryBoxFuture<'static, ()> + Send + Sync>;
pub type OnRetryAttemptStartFn = Arc<dyn Fn() -> RetryBoxFuture<'static, ()> + Send + Sync>;
pub type OnRetryFinishedFn =
    Arc<dyn Fn(bool, u32, Option<String>) -> RetryBoxFuture<'static, ()> + Send + Sync>;

/// `retry_assistant_call` 的生命周期回调集合（对照 `RetryCallbacks`）。
#[derive(Clone, Default)]
pub struct RetryCallbacks {
    pub on_retry_scheduled: Option<OnRetryScheduledFn>,
    pub on_retry_attempt_start: Option<OnRetryAttemptStartFn>,
    pub on_retry_finished: Option<OnRetryFinishedFn>,
}

fn non_retryable_limit_regex() -> &'static Regex {
    static LOCK: OnceLock<Regex> = OnceLock::new();
    LOCK.get_or_init(|| {
        Regex::new(
            r"(?i)GoUsageLimitError|FreeUsageLimitError|Monthly usage limit reached|available balance|insufficient_quota|out of budget|quota exceeded|billing|subscription_sharing_usage_limit_exceeded|subscription_sharing_workplace_individual_usage_limit_exceeded",
        )
        .expect("valid non-retryable regex")
    })
}

fn zhipu_1308_prefix_regex() -> &'static Regex {
    static LOCK: OnceLock<Regex> = OnceLock::new();
    LOCK.get_or_init(|| Regex::new(r#"(?i)"code"\s*:\s*"?1308"#).expect("valid 1308 regex"))
}

fn matches_non_retryable_provider_limit(text: &str) -> bool {
    if non_retryable_limit_regex().is_match(text) {
        return true;
    }
    // 对照 `"code"\s*:\s*"?1308(?![0-9])`
    zhipu_1308_prefix_regex().find_iter(text).any(|m| {
        !text[m.end()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit())
    })
}

fn retryable_error_regex() -> &'static Regex {
    static LOCK: OnceLock<Regex> = OnceLock::new();
    LOCK.get_or_init(|| {
        let patterns = [
            "overloaded",
            "server_busy",
            "servers are currently busy",
            "currently experiencing high demand",
            "model is at capacity",
            "rate.?limit",
            "too many requests",
            "service.?(?:is.?)?(?:temporarily.?)?unavailable",
            "server.?error",
            "internal.?error",
            "provider.?returned.?(?:an.?)?error",
            "exceeded request buffer limit while retrying upstream",
            "unable to forward upstream",
            "upstream.?request.?failed",
            "upstream_stream_read_error",
            "stream was interrupted",
            "http/?2 stream failed",
            "network.?error",
            "connection.?error",
            "connection.?refused",
            "connection.?lost",
            "other side closed",
            "fetch failed",
            r"request to \S+ failed, reason:",
            "getaddrinfo",
            "ENOTFOUND",
            "EAI_AGAIN",
            "upstream.?connect",
            "reset before headers",
            "socket hang up",
            "socket connection was closed",
            "timed? out",
            "timeout",
            "terminated",
            "websocket.?closed",
            "websocket.?error",
            "ended without",
            "stream ended before message_stop",
            "stream ended before a terminal response event",
            "http2 request did not get a response",
            "stream disconnected before completion",
            "pending stream has been canceled",
            "retry delay",
            "you can retry your request",
            "try your request again",
            "please retry your request",
            "ResourceExhausted",
            "subscription_sharing_usage_unavailable",
            "subscription_sharing_user_unavailable",
        ];
        Regex::new(&format!("(?i){}", patterns.join("|"))).expect("valid retryable regex")
    })
}

fn status_code_regex() -> &'static Regex {
    static LOCK: OnceLock<Regex> = OnceLock::new();
    LOCK.get_or_init(|| Regex::new(r"429|500|502|503|504|520|524").expect("valid status regex"))
}

fn matches_retryable_status_code(text: &str) -> bool {
    // 对照 `(?<![a-z0-9])(?:429|500|502|503|504|520|524)(?![a-z0-9])`
    status_code_regex().find_iter(text).any(|m| {
        let prev_ok = text[..m.start()]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        let next_ok = text[m.end()..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        prev_ok && next_ok
    })
}

/// 判定失败的助手回复是否属于可重试的供应商/传输瞬态错误（对照 `isRetryableAssistantError`）。
pub fn is_retryable_assistant_error(message: &AssistantMessage) -> bool {
    if message.stop_reason != StopReason::Error {
        return false;
    }
    let Some(error_message) = message.error_message.as_deref() else {
        return false;
    };
    if error_message.is_empty() {
        return false;
    }
    if matches_non_retryable_provider_limit(error_message) {
        return false;
    }
    retryable_error_regex().is_match(error_message) || matches_retryable_status_code(error_message)
}

async fn sleep_with_cancel(
    delay_ms: u64,
    cancel_token: Option<&CancellationToken>,
) -> Result<(), ()> {
    if let Some(token) = cancel_token {
        if token.is_cancelled() {
            return Err(());
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(delay_ms)) => Ok(()),
            _ = token.cancelled() => Err(()),
        }
    } else {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        Ok(())
    }
}

/// 按指数退避策略包装单次生成 `AssistantMessage` 的异步调用（对照 `retryAssistantCall`）。
pub async fn retry_assistant_call<F, Fut>(
    mut produce: F,
    policy: Option<&RetryPolicy>,
    cancel_token: Option<CancellationToken>,
    callbacks: Option<&RetryCallbacks>,
) -> AssistantMessage
where
    F: FnMut() -> Fut,
    Fut: Future<Output = AssistantMessage>,
{
    let max_attempts = match policy {
        Some(p) if p.enabled => p.max_retries,
        _ => 0,
    };

    let mut attempt: u32 = 0;
    let mut last_retry: Option<(u32, String)> = None;

    loop {
        let response = produce().await;

        if response.stop_reason == StopReason::Aborted {
            if let Some((last_attempt, _)) = &last_retry
                && let Some(cb) = callbacks.and_then(|c| c.on_retry_finished.as_ref())
            {
                cb(false, *last_attempt, None).await;
            }
            return response;
        }

        if response.stop_reason != StopReason::Error {
            if let Some((last_attempt, _)) = &last_retry
                && let Some(cb) = callbacks.and_then(|c| c.on_retry_finished.as_ref())
            {
                cb(true, *last_attempt, None).await;
            }
            return response;
        }

        if attempt >= max_attempts || !is_retryable_assistant_error(&response) {
            if let Some((last_attempt, _)) = &last_retry
                && let Some(cb) = callbacks.and_then(|c| c.on_retry_finished.as_ref())
            {
                cb(false, *last_attempt, response.error_message.clone()).await;
            }
            return response;
        }

        attempt += 1;
        let err_msg = response
            .error_message
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Unknown error".to_owned());
        last_retry = Some((attempt, err_msg.clone()));

        let delay_ms = retry_delay_ms(policy.expect("policy checked above"), attempt);
        if let Some(cb) = callbacks.and_then(|c| c.on_retry_scheduled.as_ref()) {
            cb(attempt, max_attempts, delay_ms, err_msg.clone()).await;
        }

        if sleep_with_cancel(delay_ms, cancel_token.as_ref())
            .await
            .is_err()
        {
            if let Some(cb) = callbacks.and_then(|c| c.on_retry_finished.as_ref()) {
                cb(false, attempt, Some(err_msg)).await;
            }
            let mut aborted = response;
            aborted.stop_reason = StopReason::Aborted;
            aborted.error_message = None;
            return aborted;
        }

        if let Some(cb) = callbacks.and_then(|c| c.on_retry_attempt_start.as_ref()) {
            cb().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::faux::faux_text_message;

    #[test]
    fn retry_classifier_respects_quota_exclusions_and_status_code_boundaries() {
        let mut msg = faux_text_message("");
        msg.stop_reason = StopReason::Error;

        // Quota / plan limits must NOT be retried even when accompanied by 429
        for non_retryable in [
            "429 GoUsageLimitError: limit reached",
            "429 FreeUsageLimitError",
            "Monthly usage limit reached, enable available balance",
            "Error: insufficient_quota",
            "429 {\"error\":{\"code\":\"1308\",\"message\":\"Usage limit reached\"}}",
            "429 {\"code\": 1308, \"message\": \"plan limit\"}",
        ] {
            msg.error_message = Some(non_retryable.to_owned());
            assert!(
                !is_retryable_assistant_error(&msg),
                "expected non-retryable for: {non_retryable}"
            );
        }

        // Error code 13080 (different number) with 429 IS retryable
        msg.error_message = Some("429 {\"code\": 13080, \"message\": \"busy\"}".to_owned());
        assert!(is_retryable_assistant_error(&msg));

        // Status code embedded in request ID or byte count must NOT trigger status match
        msg.error_message =
            Some("Invalid parameter in request req_4290abc (size 15002 bytes)".to_owned());
        assert!(!is_retryable_assistant_error(&msg));

        // Standalone status codes and transient errors ARE retryable
        for retryable in [
            "HTTP 429: Too Many Requests",
            "status 503 Service Temporarily Unavailable",
            "502 Bad Gateway",
            "stream disconnected before completion: stream closed",
            "Anthropic stream ended before message_stop",
            "fetch failed: socket hang up",
        ] {
            msg.error_message = Some(retryable.to_owned());
            assert!(
                is_retryable_assistant_error(&msg),
                "expected retryable for: {retryable}"
            );
        }
    }

    #[tokio::test]
    async fn retry_assistant_call_retries_transient_errors_and_normalizes_sleep_abort() {
        let calls = Arc::new(AtomicUsize::new(0));
        let scheduled = Arc::new(Mutex::new(Vec::<(u32, u64)>::new()));
        let finished = Arc::new(Mutex::new(Vec::<(bool, u32)>::new()));

        let policy = RetryPolicy {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 5,
            max_agent_delay_ms: Some(20),
        };

        let calls_c = calls.clone();
        let sched_c = scheduled.clone();
        let fin_c = finished.clone();
        let callbacks = RetryCallbacks {
            on_retry_scheduled: Some(Arc::new(move |attempt, _max, delay, _err| {
                sched_c.lock().unwrap().push((attempt, delay));
                Box::pin(async {})
            })),
            on_retry_attempt_start: None,
            on_retry_finished: Some(Arc::new(move |ok, attempt, _err| {
                fin_c.lock().unwrap().push((ok, attempt));
                Box::pin(async {})
            })),
        };

        let res = retry_assistant_call(
            move || {
                let n = calls_c.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n < 2 {
                        let mut err = faux_text_message("");
                        err.stop_reason = StopReason::Error;
                        err.error_message = Some("503 Service Unavailable".to_owned());
                        err
                    } else {
                        faux_text_message("succeeded on 3rd try")
                    }
                }
            },
            Some(&policy),
            None,
            Some(&callbacks),
        )
        .await;

        assert_eq!(res.stop_reason, StopReason::Stop);
        assert_eq!(res.text(), "succeeded on 3rd try");
        assert_eq!(*scheduled.lock().unwrap(), vec![(1, 5), (2, 10)]);
        assert_eq!(*finished.lock().unwrap(), vec![(true, 2)]);

        // Now test cancellation during backoff sleep: must normalize to StopReason::Aborted without error_message
        let cancel = CancellationToken::new();
        let cancel_trigger = cancel.clone();
        let slow_policy = RetryPolicy {
            enabled: true,
            max_retries: 2,
            base_delay_ms: 5_000,
            max_agent_delay_ms: Some(10_000),
        };
        let abort_res = retry_assistant_call(
            move || {
                let cancel_trigger = cancel_trigger.clone();
                async move {
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        cancel_trigger.cancel();
                    });
                    let mut err = faux_text_message("");
                    err.stop_reason = StopReason::Error;
                    err.error_message = Some("429 rate limit".to_owned());
                    err
                }
            },
            Some(&slow_policy),
            Some(cancel),
            None,
        )
        .await;

        assert_eq!(abort_res.stop_reason, StopReason::Aborted);
        assert!(abort_res.error_message.is_none());
    }
}

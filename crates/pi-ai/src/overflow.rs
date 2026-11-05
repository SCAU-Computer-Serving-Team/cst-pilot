use std::sync::OnceLock;

use regex::Regex;

use crate::types::{AssistantMessage, StopReason};

const OVERFLOW_PATTERN_STRINGS: &[&str] = &[
    r"(?i)prompt (?:is )?too long",
    r"(?i)prompt exceeds max length",
    r"(?i)request_too_large",
    r"(?i)input is too long for requested model",
    r"(?i)exceeds the context window",
    r"(?i)exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))",
    r"(?i)context window exceeded",
    r"(?i)input token count.*exceeds the maximum",
    r"(?i)maximum prompt length is \d+",
    r"(?i)reduce the length of the messages",
    r"(?i)maximum context length is \d+ tokens",
    r"(?i)exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?",
    r"(?i)input \(\d+ tokens\) is longer than the model'?s context length \(\d+ tokens\)",
    r"(?i)exceeds the limit of \d+",
    r"(?i)exceeds the available context size",
    r"(?i)greater than the context length",
    r"(?i)context window exceeds limit",
    r"(?i)exceeded model token limit",
    r"(?i)too large for model with \d+ maximum context length",
    r"(?i)prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?",
    r"(?i)model_context_window_exceeded",
    r"(?i)prompt too long; exceeded (?:max )?context length",
    r"(?i)range of input length should be",
    r"(?i)context[_ ]length[_ ]exceeded",
    r"(?i)too many tokens",
    r"(?i)token limit exceeded",
    r"(?i)^(?:[\w -]+ API error \(413\):|413(?:\s|:|$))",
];

const NON_OVERFLOW_PATTERN_STRINGS: &[&str] = &[
    r"(?i)^(Throttling error|Service unavailable):",
    r"(?i)rate limit",
    r"(?i)too many requests",
];

fn compiled_overflow_patterns() -> &'static Vec<Regex> {
    static LOCK: OnceLock<Vec<Regex>> = OnceLock::new();
    LOCK.get_or_init(|| {
        OVERFLOW_PATTERN_STRINGS
            .iter()
            .map(|p| Regex::new(p).expect("valid overflow regex"))
            .collect()
    })
}

fn compiled_non_overflow_patterns() -> &'static Vec<Regex> {
    static LOCK: OnceLock<Vec<Regex>> = OnceLock::new();
    LOCK.get_or_init(|| {
        NON_OVERFLOW_PATTERN_STRINGS
            .iter()
            .map(|p| Regex::new(p).expect("valid non-overflow regex"))
            .collect()
    })
}

fn compiled_cerebras_bodyless_pattern() -> &'static Regex {
    static LOCK: OnceLock<Regex> = OnceLock::new();
    LOCK.get_or_init(|| {
        Regex::new(r"(?i)^4(?:00|13)\s*(?:status code)?\s*\(no body\)")
            .expect("valid cerebras regex")
    })
}

/// 返回用于检测上下文溢出的全部正则表达式切片（对照 `getOverflowPatterns`）。
pub fn get_overflow_patterns() -> &'static [Regex] {
    compiled_overflow_patterns().as_slice()
}

/// 检测助手回复是否表示上下文窗口溢出（对照 `isContextOverflow`）。
///
/// 覆盖三类溢出场景：
/// 1. 供应商返回 `stop_reason == Error` 且 `error_message` 命中溢出正则（同时排除限流错误）。
/// 2. 静默溢出（如 z.ai）：`stop_reason == Stop` 但 `usage.input + usage.cache_read > context_window`。
/// 3. 长度截断溢出（如小米 MiMo）：`stop_reason == Length`、`usage.output == 0` 且输入占满窗口（`>= 0.99 * context_window`）。
pub fn is_context_overflow(message: &AssistantMessage, context_window: Option<u64>) -> bool {
    if message.stop_reason == StopReason::Error
        && let Some(err_msg) = &message.error_message
        && !err_msg.is_empty()
    {
        let is_non_overflow = compiled_non_overflow_patterns()
            .iter()
            .any(|p| p.is_match(err_msg));
        if !is_non_overflow {
            if compiled_overflow_patterns()
                .iter()
                .any(|p| p.is_match(err_msg))
            {
                return true;
            }
            if message.provider == "cerebras"
                && compiled_cerebras_bodyless_pattern().is_match(err_msg)
            {
                return true;
            }
        }
    }

    if let Some(window) = context_window.filter(|&w| w > 0) {
        let input_tokens = message.usage.input.saturating_add(message.usage.cache_read);
        if message.stop_reason == StopReason::Stop && input_tokens > window {
            return true;
        }
        if message.stop_reason == StopReason::Length && message.usage.output == 0 {
            let threshold = (window as f64) * 0.99;
            if (input_tokens as f64) >= threshold {
                return true;
            }
        }
    }

    false
}

/// 检测 `Length` 结束是否提前截断在预期最大输出 Token 之下（对照 `isRecoverableLength`）。
pub fn is_recoverable_length(message: &AssistantMessage, desired_max_output: u64) -> bool {
    message.stop_reason == StopReason::Length
        && desired_max_output > 0
        && message.usage.output < desired_max_output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::faux::faux_text_message;

    #[test]
    fn detects_provider_error_patterns_and_excludes_throttling() {
        let mut msg = faux_text_message("");
        msg.stop_reason = StopReason::Error;

        let positive_cases = [
            "prompt is too long: 213462 tokens > 200000 maximum",
            "Prompt too long",
            "Prompt exceeds max length",
            "413 {\"error\":{\"type\":\"request_too_large\"}}",
            "Your input exceeds the context window of this model",
            "Requested token count exceeds the model's maximum context length of 131,072 tokens",
            "Input length (265330) exceeds model's maximum context length (262144).",
            "litellm.ContextWindowExceededError: Context Window exceeded for given call.",
            "The input token count (1196265) exceeds the maximum number of tokens allowed (1048575)",
            "This model's maximum prompt length is 131072 but the request contains 537812 tokens",
            "Please reduce the length of the messages or completion",
            "This endpoint's maximum context length is 32768 tokens.",
            "Input length 50000 exceeds the maximum allowed input length of 32,768 tokens.",
            "The input (40000 tokens) is longer than the model's context length (32768 tokens).",
            "prompt token count of 150000 exceeds the limit of 128000",
            "github-copilot API error (413): {\"message\":\"failed to parse request\"}",
            "413 Payload Too Large",
            "Range of input length should be [1, 131072]",
            "invalid params, context window exceeds limit",
            "Your request exceeded model token limit: 128000",
        ];

        for text in positive_cases {
            msg.error_message = Some(text.to_owned());
            assert!(
                is_context_overflow(&msg, None),
                "expected overflow for error: {text}"
            );
        }

        // Cerebras bodyless 400/413 only matches when provider == "cerebras"
        msg.error_message = Some("400 status code (no body)".to_owned());
        msg.provider = "openai".to_owned();
        assert!(!is_context_overflow(&msg, None));
        msg.provider = "cerebras".to_owned();
        assert!(is_context_overflow(&msg, None));

        // Throttling / rate-limit errors must be excluded even if they mention "too many tokens"
        msg.provider = "bedrock".to_owned();
        msg.error_message =
            Some("Throttling error: Too many tokens, please wait before trying again.".to_owned());
        assert!(!is_context_overflow(&msg, None));
        msg.error_message = Some("Rate limit reached: too many tokens per minute".to_owned());
        assert!(!is_context_overflow(&msg, None));
    }

    #[test]
    fn detects_silent_overflow_and_mimo_length_overflow_and_recoverable_length() {
        let mut msg = faux_text_message("ok");
        msg.stop_reason = StopReason::Stop;
        msg.usage.input = 110_000;
        msg.usage.cache_read = 20_000;
        assert!(is_context_overflow(&msg, Some(128_000)));
        assert!(!is_context_overflow(&msg, Some(140_000)));

        // Xiaomi MiMo: length stop + 0 output + input >= 99% of context_window
        let mut mimo = faux_text_message("");
        mimo.stop_reason = StopReason::Length;
        mimo.usage.input = 99_500;
        mimo.usage.output = 0;
        assert!(is_context_overflow(&mimo, Some(100_000)));
        mimo.usage.input = 90_000;
        assert!(!is_context_overflow(&mimo, Some(100_000)));

        // Recoverable length
        mimo.usage.output = 512;
        assert!(is_recoverable_length(&mimo, 4096));
        assert!(!is_recoverable_length(&mimo, 512));
        assert!(!is_recoverable_length(&mimo, 0));
    }
}

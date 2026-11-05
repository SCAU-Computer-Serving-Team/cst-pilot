use std::collections::{BTreeMap, VecDeque};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll, Waker};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use futures_core::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

pub type JsonValue = Value;
pub type JsonObject = Map<String, Value>;
pub type ProviderId = String;
pub type Api = String;
pub type ClassifierApi = String;
pub type ProviderEnv = BTreeMap<String, String>;
pub type ProviderHeaders = BTreeMap<String, Option<String>>;
pub type SamplingParams = BTreeMap<String, Value>;

/// 已知的四大对话协议标识常量。
pub mod known_api {
    pub const OPENAI_RESPONSES: &str = "openai-responses";
    pub const OPENAI_COMPLETIONS: &str = "openai-completions";
    pub const ANTHROPIC_MESSAGES: &str = "anthropic-messages";
    pub const GOOGLE_GENERATIVE_AI: &str = "google-generative-ai";
}

/// 已知的分类器协议标识常量。
pub mod known_classifier_api {
    pub const TYPESAFE_SYSTEM_ONE: &str = "typesafe-system-one";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    Minimal,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    Max,
}

impl ThinkingLevel {
    pub const ALL: [ThinkingLevel; 6] = [
        ThinkingLevel::Minimal,
        ThinkingLevel::Low,
        ThinkingLevel::Medium,
        ThinkingLevel::High,
        ThinkingLevel::XHigh,
        ThinkingLevel::Max,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            ThinkingLevel::Minimal => "minimal",
            ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
            ThinkingLevel::XHigh => "xhigh",
            ThinkingLevel::Max => "max",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    Max,
}

impl ModelThinkingLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            ModelThinkingLevel::Off => "off",
            ModelThinkingLevel::Minimal => "minimal",
            ModelThinkingLevel::Low => "low",
            ModelThinkingLevel::Medium => "medium",
            ModelThinkingLevel::High => "high",
            ModelThinkingLevel::XHigh => "xhigh",
            ModelThinkingLevel::Max => "max",
        }
    }
}

impl From<ThinkingLevel> for ModelThinkingLevel {
    fn from(level: ThinkingLevel) -> Self {
        match level {
            ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
            ThinkingLevel::Low => ModelThinkingLevel::Low,
            ThinkingLevel::Medium => ModelThinkingLevel::Medium,
            ThinkingLevel::High => ModelThinkingLevel::High,
            ThinkingLevel::XHigh => ModelThinkingLevel::XHigh,
            ThinkingLevel::Max => ModelThinkingLevel::Max,
        }
    }
}

/// 模型思考档位映射表：
/// - 键不存在：使用协议默认映射；
/// - 键对应 `None`（JSON `null`）：模型不支持该思考档位；
/// - 键对应 `Some(String)`：映射为服务商侧的具体字符串值。
pub type ThinkingLevelMap = BTreeMap<ModelThinkingLevel, Option<String>>;
pub type SamplingParamsByThinkingLevel = BTreeMap<ModelThinkingLevel, SamplingParams>;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingBudgets {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimal: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub low: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub medium: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub high: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelInputModality {
    Text,
    Image,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheRetention {
    None,
    #[default]
    Short,
    Long,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPromptCache {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub long: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostRates {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostTier {
    pub input_tokens_above: u64,
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<ModelCostTier>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelImageResizeOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jpeg_quality: Option<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelImageInputLimits {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resize: Option<ModelImageResizeOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_per_message: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_per_request: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInputLimits {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_request_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<ModelImageInputLimits>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionAffinityFormat {
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "openai-nosession")]
    OpenAiNoSession,
    #[serde(rename = "openrouter")]
    OpenRouter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThinkingTokenBudgetField {
    #[serde(rename = "thinking_token_budget")]
    ThinkingTokenBudget,
    #[serde(rename = "thinking_budget")]
    ThinkingBudget,
    #[serde(rename = "thinking_budget_tokens")]
    ThinkingBudgetTokens,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnthropicAllowedFallbackModel {
    pub provider: String,
    pub model: String,
    pub cost: ModelCost,
}

/// 统一的模型/服务商兼容配置，覆盖四大对话协议与 System One 分类器协议的兼容开关。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCompat {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_store: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_developer_role: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_reasoning_effort: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_usage_in_streaming: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_finish_reason: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens_field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_tool_result_name: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_assistant_after_tool_result: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_thinking_as_text: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_reasoning_content_on_assistant_messages: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_template_kwargs: Option<BTreeMap<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_template_args: Option<BTreeMap<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_router_routing: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vercel_gateway_routing: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zai_tool_stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_token_budget_field: Option<ThinkingTokenBudgetField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_thinking_token_budget: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_open_ai_grammar_tools: Option<bool>,
    #[serde(
        rename = "supportsOpenAIGrammarTools",
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_openai_grammar_tools_raw: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_system_messages: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_tool_additions: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_strict_mode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_session_affinity_headers: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_affinity_format: Option<SessionAffinityFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_long_cache_retention: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vllm_priority: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_additional_tools: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_tool_search: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_explicit_prompt_cache_mode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_effort: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_max_output_tokens: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_eager_tool_input_streaming: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_cache_control_on_tools: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_temperature: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub force_adaptive_thinking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_empty_signature: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_strict_tools: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_tool_changes: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_fallback_models: Option<Vec<AnthropicAllowedFallbackModel>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_input: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_image_opt_in_header: Option<bool>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl ProviderCompat {
    pub fn supports_grammar_tools(&self) -> bool {
        self.supports_openai_grammar_tools_raw
            .or(self.supports_open_ai_grammar_tools)
            .unwrap_or(false)
    }
}

pub type OpenAICompletionsCompat = ProviderCompat;
pub type OpenAIResponsesCompat = ProviderCompat;
pub type AnthropicMessagesCompat = ProviderCompat;
pub type SystemOneCompat = ProviderCompat;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextSignatureV1 {
    pub v: u8,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextContent {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_signature: Option<String>,
}

impl TextContent {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            text_signature: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingContent {
    pub thinking: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redacted: Option<bool>,
}

impl ThinkingContent {
    pub fn new(thinking: impl Into<String>) -> Self {
        Self {
            thinking: thinking.into(),
            thinking_signature: None,
            redacted: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageContent {
    pub data: String,
    pub mime_type: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: JsonObject,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments_error: Option<String>,
}

/// 助手消息中的内容块（`text` / `thinking` / `toolCall`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text(TextContent),
    #[serde(rename = "thinking")]
    Thinking(ThinkingContent),
    #[serde(rename = "toolCall")]
    ToolCall(ToolCall),
}

impl ContentBlock {
    pub fn as_text(&self) -> Option<&TextContent> {
        match self {
            ContentBlock::Text(t) => Some(t),
            _ => None,
        }
    }

    pub fn as_thinking(&self) -> Option<&ThinkingContent> {
        match self {
            ContentBlock::Thinking(t) => Some(t),
            _ => None,
        }
    }

    pub fn as_tool_call(&self) -> Option<&ToolCall> {
        match self {
            ContentBlock::ToolCall(tc) => Some(tc),
            _ => None,
        }
    }
}

/// 用户消息或工具结果中的内容块（`text` / `image`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum UserContentBlock {
    #[serde(rename = "text")]
    Text(TextContent),
    #[serde(rename = "image")]
    Image(ImageContent),
}

impl UserContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        UserContentBlock::Text(TextContent::new(text))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Blocks(Vec<UserContentBlock>),
}

impl From<&str> for UserContent {
    fn from(s: &str) -> Self {
        UserContent::Text(s.to_owned())
    }
}

impl From<String> for UserContent {
    fn from(s: String) -> Self {
        UserContent::Text(s)
    }
}

impl From<Vec<UserContentBlock>> for UserContent {
    fn from(blocks: Vec<UserContentBlock>) -> Self {
        UserContent::Blocks(blocks)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SystemTextBlock {
    #[serde(rename = "text")]
    Text(TextContent),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemContent {
    Text(String),
    Blocks(Vec<SystemTextBlock>),
}

impl SystemContent {
    pub fn as_plain_text(&self) -> String {
        match self {
            SystemContent::Text(s) => s.clone(),
            SystemContent::Blocks(blocks) => blocks
                .iter()
                .map(|SystemTextBlock::Text(t)| t.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

impl From<&str> for SystemContent {
    fn from(s: &str) -> Self {
        SystemContent::Text(s.to_owned())
    }
}

impl From<String> for SystemContent {
    fn from(s: String) -> Self {
        SystemContent::Text(s)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub total: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_from_provider: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_1h: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    pub total_tokens: u64,
    pub cost: UsageCost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum StopReason {
    #[default]
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "stop")]
    Stop,
    #[serde(rename = "length")]
    Length,
    #[serde(rename = "toolUse")]
    ToolUse,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "aborted")]
    Aborted,
    #[serde(rename = "deferred")]
    Deferred,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeferredHandle {
    pub provider: String,
    pub model_id: String,
    pub api: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll_after_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<JsonValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessageDiagnostic {
    pub kind: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedToolCallRecord {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<JsonObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments_bytes: Option<usize>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedToolCalls {
    pub calls: Vec<NestedToolCallRecord>,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolReference {
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum GrammarFormat {
    #[serde(rename = "openai_lark")]
    OpenAiLark,
    #[serde(rename = "openai_regex")]
    OpenAiRegex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ConstrainedSamplingConfig {
    #[serde(rename = "json_schema")]
    JsonSchema { strict: String },
    #[serde(rename = "grammar")]
    Grammar {
        variants: BTreeMap<GrammarFormat, String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolConstrainedSampling {
    Disabled(bool),
    Config(ConstrainedSamplingConfig),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: JsonValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constrained_sampling: Option<ToolConstrainedSampling>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemMessage {
    pub content: SystemContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sections: Option<BTreeMap<String, Option<String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools_added: Option<Vec<Tool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools_removed: Option<Vec<ToolReference>>,
    pub timestamp: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMessage {
    pub content: UserContent,
    pub timestamp: u64,
}

impl UserMessage {
    pub fn new(content: impl Into<UserContent>) -> Self {
        Self {
            content: content.into(),
            timestamp: now_ms(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    pub content: Vec<ContentBlock>,
    pub api: Api,
    pub provider: ProviderId,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_thinking_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ModelThinkingLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<AssistantMessageDiagnostic>>,
    pub usage: Usage,
    pub stop_reason: StopReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deferred: Option<DeferredHandle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_stop_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_turn: Option<bool>,
    pub timestamp: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

impl AssistantMessage {
    pub fn new_pending(model: &Model) -> Self {
        Self {
            content: Vec::new(),
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Pending,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: now_ms(),
            duration_ms: None,
        }
    }

    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn thinking(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Thinking(t) => Some(t.thinking.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn tool_calls(&self) -> Vec<&ToolCall> {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolCall(tc) => Some(tc),
                _ => None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultMessage {
    pub tool_call_id: String,
    pub tool_name: String,
    pub content: Vec<UserContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nested_calls: Option<NestedToolCalls>,
    pub is_error: bool,
    pub timestamp: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// 统一对话历史消息枚举，按 `role` 标签序列化。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role")]
pub enum Message {
    #[serde(rename = "system")]
    System(SystemMessage),
    #[serde(rename = "user")]
    User(UserMessage),
    #[serde(rename = "assistant")]
    Assistant(AssistantMessage),
    #[serde(rename = "toolResult")]
    ToolResult(ToolResultMessage),
}

impl Message {
    pub fn role(&self) -> &'static str {
        match self {
            Message::System(_) => "system",
            Message::User(_) => "user",
            Message::Assistant(_) => "assistant",
            Message::ToolResult(_) => "toolResult",
        }
    }
}

/// 公开请求上下文（`system_prompt` 与 `tools` 会在进入协议层前由 `normalize_context` 折叠为首条 `SystemMessage`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
}

/// 归一化后的对话上下文，仅由 `normalize_context` / `resolve_transcript` 产出。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TranscriptContext {
    pub(crate) messages: Vec<Message>,
}

impl TranscriptContext {
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn into_messages(self) -> Vec<Message> {
        self.messages
    }
}

/// 流式响应事件，字段与上游 `AssistantMessageEvent` 完全对齐。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AssistantMessageEvent {
    #[serde(rename = "start")]
    Start { partial: AssistantMessage },
    #[serde(rename = "text_start", rename_all = "camelCase")]
    TextStart {
        content_index: usize,
        partial: AssistantMessage,
    },
    #[serde(rename = "text_delta", rename_all = "camelCase")]
    TextDelta {
        content_index: usize,
        delta: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "text_end", rename_all = "camelCase")]
    TextEnd {
        content_index: usize,
        content: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "thinking_start", rename_all = "camelCase")]
    ThinkingStart {
        content_index: usize,
        partial: AssistantMessage,
    },
    #[serde(rename = "thinking_delta", rename_all = "camelCase")]
    ThinkingDelta {
        content_index: usize,
        delta: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "thinking_end", rename_all = "camelCase")]
    ThinkingEnd {
        content_index: usize,
        content: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_start", rename_all = "camelCase")]
    ToolCallStart {
        content_index: usize,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_delta", rename_all = "camelCase")]
    ToolCallDelta {
        content_index: usize,
        delta: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_end", rename_all = "camelCase")]
    ToolCallEnd {
        content_index: usize,
        tool_call: ToolCall,
        partial: AssistantMessage,
    },
    #[serde(rename = "done")]
    Done {
        reason: StopReason,
        message: AssistantMessage,
    },
    #[serde(rename = "error")]
    Error {
        reason: StopReason,
        error: AssistantMessage,
    },
}

/// 对话模型（Chat Model）定义。
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub name: String,
    pub api: Api,
    pub provider: ProviderId,
    pub base_url: String,
    pub input: Vec<ModelInputModality>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<ModelInputLimits>,
    pub cost: ModelCost,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub model_type: Option<String>,
    pub reasoning: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<ModelPromptCache>,
    pub context_window: u64,
    pub max_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling_params: Option<SamplingParams>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling_params_by_thinking_level: Option<SamplingParamsByThinkingLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<ProviderCompat>,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted_headers: Option<BTreeMap<&str, &str>> = self
            .headers
            .as_ref()
            .map(|h| h.keys().map(|k| (k.as_str(), "[REDACTED]")).collect());
        f.debug_struct("Model")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("api", &self.api)
            .field("provider", &self.provider)
            .field("base_url", &self.base_url)
            .field("input", &self.input)
            .field("input_limits", &self.input_limits)
            .field("cost", &self.cost)
            .field("headers", &redacted_headers)
            .field("model_type", &self.model_type)
            .field("reasoning", &self.reasoning)
            .field("thinking_level_map", &self.thinking_level_map)
            .field("prompt_cache", &self.prompt_cache)
            .field("context_window", &self.context_window)
            .field("max_tokens", &self.max_tokens)
            .field("sampling_params", &self.sampling_params)
            .field(
                "sampling_params_by_thinking_level",
                &self.sampling_params_by_thinking_level,
            )
            .field("compat", &self.compat)
            .finish()
    }
}

impl Model {
    pub fn supports_image_input(&self) -> bool {
        self.input.contains(&ModelInputModality::Image)
    }
}

/// 结构化分类器模型（Classifier Model）定义。
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierModel {
    #[serde(rename = "type", default = "default_classifier_type")]
    pub model_type: String,
    pub id: String,
    pub name: String,
    pub api: ClassifierApi,
    pub provider: ProviderId,
    pub base_url: String,
    pub input: Vec<ModelInputModality>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<ModelInputLimits>,
    pub cost: ModelCost,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    pub context_window: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<SystemOneCompat>,
}

impl std::fmt::Debug for ClassifierModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted_headers: Option<BTreeMap<&str, &str>> = self
            .headers
            .as_ref()
            .map(|h| h.keys().map(|k| (k.as_str(), "[REDACTED]")).collect());
        f.debug_struct("ClassifierModel")
            .field("model_type", &self.model_type)
            .field("id", &self.id)
            .field("name", &self.name)
            .field("api", &self.api)
            .field("provider", &self.provider)
            .field("base_url", &self.base_url)
            .field("input", &self.input)
            .field("input_limits", &self.input_limits)
            .field("cost", &self.cost)
            .field("headers", &redacted_headers)
            .field("context_window", &self.context_window)
            .field("compat", &self.compat)
            .finish()
    }
}

fn default_classifier_type() -> String {
    "classifier".to_owned()
}

impl ClassifierModel {
    pub fn supports_image_input(&self) -> bool {
        self.input.contains(&ModelInputModality::Image)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelType {
    Chat,
    Classifier,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum AnyModel {
    Chat(Model),
    Classifier(ClassifierModel),
}

impl<'de> Deserialize<'de> for AnyModel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let obj = value
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("model definition must be a JSON object"))?;

        let explicit_type = obj.get("type").and_then(|v| v.as_str());
        let api = obj.get("api").and_then(|v| v.as_str());

        match explicit_type {
            Some("classifier") => serde_json::from_value::<ClassifierModel>(value)
                .map(AnyModel::Classifier)
                .map_err(serde::de::Error::custom),
            Some("chat") => serde_json::from_value::<Model>(value)
                .map(AnyModel::Chat)
                .map_err(serde::de::Error::custom),
            Some(other) => Err(serde::de::Error::custom(format!(
                "unsupported model type: {other}"
            ))),
            None => {
                if api == Some(known_classifier_api::TYPESAFE_SYSTEM_ONE) {
                    serde_json::from_value::<ClassifierModel>(value)
                        .map(AnyModel::Classifier)
                        .map_err(serde::de::Error::custom)
                } else {
                    serde_json::from_value::<Model>(value)
                        .map(AnyModel::Chat)
                        .map_err(serde::de::Error::custom)
                }
            }
        }
    }
}

impl AnyModel {
    pub fn id(&self) -> &str {
        match self {
            AnyModel::Chat(m) => &m.id,
            AnyModel::Classifier(m) => &m.id,
        }
    }

    pub fn provider(&self) -> &str {
        match self {
            AnyModel::Chat(m) => &m.provider,
            AnyModel::Classifier(m) => &m.provider,
        }
    }

    pub fn api(&self) -> &str {
        match self {
            AnyModel::Chat(m) => &m.api,
            AnyModel::Classifier(m) => &m.api,
        }
    }

    pub fn model_type(&self) -> ModelType {
        match self {
            AnyModel::Chat(_) => ModelType::Chat,
            AnyModel::Classifier(_) => ModelType::Classifier,
        }
    }

    pub fn as_chat(&self) -> Option<&Model> {
        match self {
            AnyModel::Chat(m) => Some(m),
            AnyModel::Classifier(_) => None,
        }
    }

    pub fn as_classifier(&self) -> Option<&ClassifierModel> {
        match self {
            AnyModel::Classifier(m) => Some(m),
            AnyModel::Chat(_) => None,
        }
    }
}

// =============================================================================
// Classifier types (TypeSafe System One)
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassifierBoolCriteria {
    #[serde(rename = "true")]
    pub r#true: String,
    #[serde(rename = "false")]
    pub r#false: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClassifierQuestion {
    #[serde(rename = "choice")]
    Choice {
        instructions: String,
        criteria: BTreeMap<String, String>,
    },
    #[serde(rename = "score")]
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
    #[serde(rename = "bool")]
    Bool {
        instructions: String,
        criteria: ClassifierBoolCriteria,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassifierContext {
    pub state: JsonObject,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageContent>>,
    pub questions: BTreeMap<String, ClassifierQuestion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClassifierAnswer {
    #[serde(rename = "choice")]
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    #[serde(rename = "score")]
    Score { score: f64, confidence: f64 },
    #[serde(rename = "bool")]
    Bool { probability: f64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClassifierStopReason {
    Stop,
    Error,
    Aborted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierResult {
    pub api: ClassifierApi,
    pub provider: ProviderId,
    pub model: String,
    pub answers: BTreeMap<String, ClassifierAnswer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub stop_reason: ClassifierStopReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub timestamp: u64,
}

// =============================================================================
// Request & Stream Options
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoice {
    Auto,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transport {
    #[serde(rename = "sse")]
    Sse,
    #[serde(rename = "websocket")]
    WebSocket,
    #[serde(rename = "websocket-cached")]
    WebSocketCached,
    #[serde(rename = "auto")]
    Auto,
}

/// 客户端身份归属元数据（对照 `utils/identity.ts` 的 `ClientAttribution`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientAttribution {
    pub name: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<String>>,
}

pub const PI_ATTRIBUTION_NAME: &str = "pi";
pub const PI_ATTRIBUTION_URL: &str = "https://pi.dev";
pub const PI_ATTRIBUTION_CATEGORY: &str = "cli-agent";

/// 返回 Pi 默认客户端身份归属配置（`{ name: "pi", url: "https://pi.dev", categories: ["cli-agent"] }`）。
pub fn pi_attribution() -> ClientAttribution {
    ClientAttribution {
        name: PI_ATTRIBUTION_NAME.to_owned(),
        url: PI_ATTRIBUTION_URL.to_owned(),
        categories: Some(vec![PI_ATTRIBUTION_CATEGORY.to_owned()]),
    }
}

fn deserialize_double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// 客户端归属信息：
    /// - `None`：未设置（合并时跳过）；
    /// - `Some(None)`：显式设为 `null`（关闭已设置的归属头）；
    /// - `Some(Some(attr))`：启用指定归属头。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_double_option"
    )]
    pub attribution: Option<Option<ClientAttribution>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl ClientIdentity {
    /// 返回包含 `pi_attribution()` 的默认身份标识。
    pub fn with_pi_attribution() -> Self {
        Self {
            attribution: Some(Some(pi_attribution())),
            ..Self::default()
        }
    }
}

/// 按字段顺序合并多个 `ClientIdentity`（后出现的非 `None` 字段覆盖前者，`attribution: Some(None)` 可显式清除归属）。
pub fn merge_client_identity<'a>(
    sources: impl IntoIterator<Item = Option<&'a ClientIdentity>>,
) -> Option<ClientIdentity> {
    let mut merged: Option<ClientIdentity> = None;
    for source in sources.into_iter().flatten() {
        let target = merged.get_or_insert_with(ClientIdentity::default);
        if source.app_name.is_some() {
            target.app_name = source.app_name.clone();
        }
        if source.app_version.is_some() {
            target.app_version = source.app_version.clone();
        }
        if source.app_url.is_some() {
            target.app_url = source.app_url.clone();
        }
        if source.user_agent.is_some() {
            target.user_agent = source.user_agent.clone();
        }
        if source.attribution.is_some() {
            target.attribution = source.attribution.clone();
        }
        if source.version.is_some() {
            target.version = source.version.clone();
        }
    }
    merged
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
}

/// Receives the fully assembled request headers, including provider authentication.
/// Return the final headers; `None` values remove headers case-insensitively.
pub type OnHeadersFn = Arc<
    dyn Fn(
            ProviderHeaders,
            Model,
        ) -> Pin<Box<dyn std::future::Future<Output = ProviderHeaders> + Send>>
        + Send
        + Sync,
>;

pub type OnPayloadFn = Arc<
    dyn Fn(JsonValue, Model) -> Pin<Box<dyn std::future::Future<Output = Option<JsonValue>> + Send>>
        + Send
        + Sync,
>;

pub type OnResponseFn = Arc<
    dyn Fn(ProviderResponse, Model) -> Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

pub type OnProviderStreamEventFn = Arc<
    dyn Fn(JsonValue, Model) -> Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync,
>;

#[derive(Clone, Default)]
pub struct StreamOptions {
    pub cancel_token: Option<CancellationToken>,
    pub api_key: Option<String>,
    pub env: Option<ProviderEnv>,
    pub headers: Option<ProviderHeaders>,
    pub identity: Option<ClientIdentity>,
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub max_retry_delay_ms: Option<u64>,
    pub temperature: Option<f64>,
    pub sampling_params: Option<SamplingParams>,
    pub max_tokens: Option<u64>,
    pub transport: Option<Transport>,
    pub cache_retention: Option<CacheRetention>,
    pub session_id: Option<String>,
    pub metadata: Option<BTreeMap<String, Value>>,
    /// OpenAI Responses / Completions reasoning effort
    pub reasoning_effort: Option<ThinkingLevel>,
    /// OpenAI Responses reasoning summary ("auto" | "detailed" | "concise")
    pub reasoning_summary: Option<String>,
    /// Service tier ("auto" | "default" | "flex" | "priority" | "fast")
    pub service_tier: Option<String>,
    pub tool_choice: Option<ToolChoice>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub on_headers: Option<OnHeadersFn>,
    pub on_payload: Option<OnPayloadFn>,
    pub on_response: Option<OnResponseFn>,
    pub on_provider_stream_event: Option<OnProviderStreamEventFn>,
}

impl std::fmt::Debug for StreamOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamOptions")
            .field("has_api_key", &self.api_key.is_some())
            .field("timeout_ms", &self.timeout_ms)
            .field("max_retries", &self.max_retries)
            .field("temperature", &self.temperature)
            .field("max_tokens", &self.max_tokens)
            .field("cache_retention", &self.cache_retention)
            .field("session_id", &self.session_id)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("tool_choice", &self.tool_choice)
            .finish()
    }
}

#[derive(Clone, Default, Debug)]
pub struct SimpleStreamOptions {
    pub base: StreamOptions,
    pub tool_choice: Option<ToolChoice>,
    pub reasoning: Option<ThinkingLevel>,
    pub thinking_budgets: Option<ThinkingBudgets>,
}

#[derive(Clone, Default)]
pub struct ClassifierOptions {
    pub cancel_token: Option<CancellationToken>,
    pub api_key: Option<String>,
    pub env: Option<ProviderEnv>,
    pub headers: Option<ProviderHeaders>,
    pub identity: Option<ClientIdentity>,
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub max_retry_delay_ms: Option<u64>,
    pub temperature: Option<f64>,
}

impl std::fmt::Debug for ClassifierOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClassifierOptions")
            .field("has_api_key", &self.api_key.is_some())
            .field(
                "header_count",
                &self.headers.as_ref().map_or(0, |h| h.len()),
            )
            .field("env_count", &self.env.as_ref().map_or(0, |e| e.len()))
            .field("timeout_ms", &self.timeout_ms)
            .field("max_retries", &self.max_retries)
            .field("temperature", &self.temperature)
            .finish()
    }
}

// =============================================================================
// AssistantMessageEventStream (对照 src/utils/event-stream.ts)
// =============================================================================

struct EventStreamState {
    queue: VecDeque<AssistantMessageEvent>,
    discard_events: bool,
    done: bool,
    final_result: Option<AssistantMessage>,
    waker: Option<Waker>,
}

struct EventStreamInner {
    started_at_ms: u64,
    started_instant: Instant,
    state: Mutex<EventStreamState>,
    result_notify: Notify,
}

/// 助手流式响应事件流。
///
/// 既可作为 `futures_core::Stream<Item = AssistantMessageEvent>` 逐条消费事件，
/// 也可直接调用 `stream.result().await` 等待最终的 `AssistantMessage`。
#[derive(Clone)]
pub struct AssistantMessageEventStream {
    inner: Arc<EventStreamInner>,
}

impl Default for AssistantMessageEventStream {
    fn default() -> Self {
        Self::new()
    }
}

impl AssistantMessageEventStream {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(EventStreamInner {
                started_at_ms: now_ms(),
                started_instant: Instant::now(),
                state: Mutex::new(EventStreamState {
                    queue: VecDeque::new(),
                    discard_events: false,
                    done: false,
                    final_result: None,
                    waker: None,
                }),
                result_notify: Notify::new(),
            }),
        }
    }

    fn stamp_duration(&self, message: &mut AssistantMessage) {
        if message.duration_ms.is_none() && message.timestamp >= self.inner.started_at_ms {
            message.duration_ms = Some(self.inner.started_instant.elapsed().as_millis() as u64);
        }
    }

    pub fn push(&self, mut event: AssistantMessageEvent) {
        let mut guard = self.inner.state.lock().expect("event stream lock poisoned");
        if guard.done {
            return;
        }

        match &mut event {
            AssistantMessageEvent::Done { message, .. } => {
                self.stamp_duration(message);
                guard.done = true;
                guard.final_result = Some(message.clone());
                self.inner.result_notify.notify_waiters();
            }
            AssistantMessageEvent::Error { error, .. } => {
                self.stamp_duration(error);
                guard.done = true;
                guard.final_result = Some(error.clone());
                self.inner.result_notify.notify_waiters();
            }
            _ => {}
        }

        if !guard.discard_events {
            guard.queue.push_back(event);
        }
        if let Some(waker) = guard.waker.take() {
            waker.wake();
        }
    }

    pub fn end(&self, result: Option<AssistantMessage>) {
        let mut guard = self.inner.state.lock().expect("event stream lock poisoned");
        if let Some(mut msg) = result {
            if !guard.done {
                self.stamp_duration(&mut msg);
            }
            if guard.final_result.is_none() {
                guard.final_result = Some(msg);
                self.inner.result_notify.notify_waiters();
            }
        }
        guard.done = true;
        if let Some(waker) = guard.waker.take() {
            waker.wake();
        }
    }

    /// 供仅等待最终结果的非流式调用（如 `complete()`）显式关闭事件缓存队列，避免累积中间快照。
    pub fn discard_queued_events(&self) {
        let mut guard = self.inner.state.lock().expect("event stream lock poisoned");
        guard.discard_events = true;
        guard.queue.clear();
        guard.queue.shrink_to_fit();
    }

    /// 当前队列中缓存的未消费事件数量。
    pub fn queued_event_count(&self) -> usize {
        self.inner
            .state
            .lock()
            .expect("event stream lock poisoned")
            .queue
            .len()
    }

    pub async fn result(&self) -> AssistantMessage {
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

impl Stream for AssistantMessageEventStream {
    type Item = AssistantMessageEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        let mut guard = self.inner.state.lock().expect("event stream lock poisoned");
        if let Some(event) = guard.queue.pop_front() {
            return Poll::Ready(Some(event));
        }
        if guard.done {
            return Poll::Ready(None);
        }
        guard.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

pub fn create_assistant_message_event_stream() -> AssistantMessageEventStream {
    AssistantMessageEventStream::new()
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 生成符合 RFC 9562 的时间有序 UUID v7 字符串（对照上游 `uuidv7()`）。
pub fn uuid_v7() -> String {
    uuid::Uuid::now_v7().to_string()
}

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use pi_ai::{
    AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, CacheRetention,
    ClientIdentity, ImageContent, JsonObject, JsonValue, Message, Model, ModelThinkingLevel,
    Models, OnHeadersFn, OnPayloadFn, OnProviderStreamEventFn, OnResponseFn, ProviderHeaders,
    SamplingParams, SimpleStreamOptions, StopReason, SystemMessage, TextContent, ThinkingBudgets,
    ThinkingLevel, Tool, ToolCall, ToolChoice, ToolConstrainedSampling, ToolResultMessage,
    TranscriptContext, Transport, Usage, UsageCost, UserContentBlock, UserMessage,
    get_current_system_message, get_current_system_prompt, get_current_tools, now_ms,
};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::validation::{
    CompileOptions, CompiledValidator, SchemaCompileError, UnresolvedRefs, compile_tool_schema,
    try_compile_schema,
};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

// =============================================================================
// 1.1 AgentMessage & CustomAgentMessage
// =============================================================================

/// 上层业务注入的自定义历史消息（如 bashExecution、notification、artifact 等）。
/// 序列化时将 `role`、`timestamp` 与 `data` 平铺在同一个 JSON 对象层级。
#[derive(Debug, Clone, PartialEq)]
pub struct CustomAgentMessage {
    pub role: String,
    pub data: JsonObject,
    pub timestamp: u64,
}

impl CustomAgentMessage {
    pub fn new(role: impl Into<String>, data: JsonObject, timestamp: u64) -> Self {
        Self {
            role: role.into(),
            data,
            timestamp,
        }
    }
}

/// Agent 上下文统一消息枚举（包含四类标准 LLM 消息与上层自定义消息）。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum AgentMessage {
    System(SystemMessage),
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
    Custom(CustomAgentMessage),
}

impl AgentMessage {
    pub fn role(&self) -> &str {
        match self {
            AgentMessage::System(_) => "system",
            AgentMessage::User(_) => "user",
            AgentMessage::Assistant(_) => "assistant",
            AgentMessage::ToolResult(_) => "toolResult",
            AgentMessage::Custom(c) => &c.role,
        }
    }

    pub fn timestamp(&self) -> u64 {
        match self {
            AgentMessage::System(m) => m.timestamp,
            AgentMessage::User(m) => m.timestamp,
            AgentMessage::Assistant(m) => m.timestamp,
            AgentMessage::ToolResult(m) => m.timestamp,
            AgentMessage::Custom(m) => m.timestamp,
        }
    }

    pub fn as_llm_message(&self) -> Option<Message> {
        match self {
            AgentMessage::System(m) => Some(Message::System(m.clone())),
            AgentMessage::User(m) => Some(Message::User(m.clone())),
            AgentMessage::Assistant(m) => Some(Message::Assistant(m.clone())),
            AgentMessage::ToolResult(m) => Some(Message::ToolResult(m.clone())),
            AgentMessage::Custom(_) => None,
        }
    }

    pub fn as_system(&self) -> Option<&SystemMessage> {
        match self {
            AgentMessage::System(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_user(&self) -> Option<&UserMessage> {
        match self {
            AgentMessage::User(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_assistant(&self) -> Option<&AssistantMessage> {
        match self {
            AgentMessage::Assistant(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_tool_result(&self) -> Option<&ToolResultMessage> {
        match self {
            AgentMessage::ToolResult(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_custom(&self) -> Option<&CustomAgentMessage> {
        match self {
            AgentMessage::Custom(m) => Some(m),
            _ => None,
        }
    }
}

impl From<Message> for AgentMessage {
    fn from(msg: Message) -> Self {
        match msg {
            Message::System(m) => AgentMessage::System(m),
            Message::User(m) => AgentMessage::User(m),
            Message::Assistant(m) => AgentMessage::Assistant(m),
            Message::ToolResult(m) => AgentMessage::ToolResult(m),
        }
    }
}

impl From<SystemMessage> for AgentMessage {
    fn from(msg: SystemMessage) -> Self {
        AgentMessage::System(msg)
    }
}

impl From<UserMessage> for AgentMessage {
    fn from(msg: UserMessage) -> Self {
        AgentMessage::User(msg)
    }
}

impl From<AssistantMessage> for AgentMessage {
    fn from(msg: AssistantMessage) -> Self {
        AgentMessage::Assistant(msg)
    }
}

impl From<ToolResultMessage> for AgentMessage {
    fn from(msg: ToolResultMessage) -> Self {
        AgentMessage::ToolResult(msg)
    }
}

impl From<CustomAgentMessage> for AgentMessage {
    fn from(msg: CustomAgentMessage) -> Self {
        AgentMessage::Custom(msg)
    }
}

impl Serialize for AgentMessage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            AgentMessage::System(m) => Message::System(m.clone()).serialize(serializer),
            AgentMessage::User(m) => Message::User(m.clone()).serialize(serializer),
            AgentMessage::Assistant(m) => Message::Assistant(m.clone()).serialize(serializer),
            AgentMessage::ToolResult(m) => Message::ToolResult(m.clone()).serialize(serializer),
            AgentMessage::Custom(c) => {
                let mut map = c.data.clone();
                map.insert("role".to_owned(), Value::String(c.role.clone()));
                map.insert("timestamp".to_owned(), Value::Number(c.timestamp.into()));
                Value::Object(map).serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for AgentMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let Some(obj) = value.as_object() else {
            return Err(serde::de::Error::custom(
                "AgentMessage must be a JSON object",
            ));
        };
        let Some(role) = obj.get("role").and_then(|v| v.as_str()) else {
            return Err(serde::de::Error::custom(
                "AgentMessage object is missing string 'role' field",
            ));
        };

        match role {
            "system" | "user" | "assistant" | "toolResult" => {
                serde_json::from_value::<Message>(value)
                    .map(AgentMessage::from)
                    .map_err(serde::de::Error::custom)
            }
            custom_role => {
                let role_owned = custom_role.to_owned();
                let timestamp = obj.get("timestamp").and_then(|v| v.as_u64()).unwrap_or(0);
                let mut data = obj.clone();
                data.remove("role");
                data.remove("timestamp");
                Ok(AgentMessage::Custom(CustomAgentMessage {
                    role: role_owned,
                    data,
                    timestamp,
                }))
            }
        }
    }
}

/// 默认的 `convertToLlm` 实现：保留四类标准 LLM 消息，过滤所有 `Custom` 消息。
pub fn default_convert_to_llm(messages: &[AgentMessage]) -> Vec<Message> {
    messages
        .iter()
        .filter_map(AgentMessage::as_llm_message)
        .collect()
}

/// 提取 `AgentMessage` 序列中的 `SystemMessage` 并重放当前生效的工具声明列表。
pub fn get_agent_current_tools(messages: &[AgentMessage]) -> Vec<Tool> {
    let sys_messages: Vec<Message> = messages
        .iter()
        .filter_map(|m| match m {
            AgentMessage::System(s) => Some(Message::System(s.clone())),
            _ => None,
        })
        .collect();
    get_current_tools(&sys_messages)
}

/// 提取 `AgentMessage` 序列中的 `SystemMessage` 并重放当前合并后的基准系统消息。
pub fn get_agent_current_system_message(messages: &[AgentMessage]) -> Option<SystemMessage> {
    let sys_messages: Vec<Message> = messages
        .iter()
        .filter_map(|m| match m {
            AgentMessage::System(s) => Some(Message::System(s.clone())),
            _ => None,
        })
        .collect();
    get_current_system_message(&sys_messages)
}

/// 提取 `AgentMessage` 序列中的 `SystemMessage` 并重放当前生效的系统提示词文本。
pub fn get_agent_current_system_prompt(messages: &[AgentMessage]) -> String {
    let sys_messages: Vec<Message> = messages
        .iter()
        .filter_map(|m| match m {
            AgentMessage::System(s) => Some(Message::System(s.clone())),
            _ => None,
        })
        .collect();
    get_current_system_prompt(&sys_messages)
}

// =============================================================================
// 1.2 Tool Execution Mode, Queue Mode, AgentTool & AgentToolResult
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolExecutionMode {
    Sequential,
    #[default]
    Parallel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum QueueMode {
    #[serde(rename = "all")]
    All,
    #[default]
    #[serde(rename = "one-at-a-time")]
    OneAtATime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolReplayPolicy {
    Safe,
    #[serde(other)]
    Never,
}

fn is_false(val: &bool) -> bool {
    !*val
}

/// 工具单次执行或增量更新产出的结果对象（对照 `AgentToolResult`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentToolResult {
    #[serde(default)]
    pub content: Vec<UserContentBlock>,
    #[serde(default = "default_empty_object_value")]
    pub details: JsonValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

fn default_empty_object_value() -> JsonValue {
    Value::Object(Map::new())
}

impl Default for AgentToolResult {
    fn default() -> Self {
        Self {
            content: Vec::new(),
            details: default_empty_object_value(),
            structured_content: None,
            usage: None,
            is_error: false,
            terminate: None,
        }
    }
}

impl AgentToolResult {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![UserContentBlock::Text(TextContent::new(text))],
            ..Self::default()
        }
    }

    pub fn error_text(message: impl Into<String>) -> Self {
        Self {
            content: vec![UserContentBlock::Text(TextContent::new(message))],
            details: Value::Object(Map::new()),
            structured_content: None,
            usage: None,
            is_error: true,
            terminate: None,
        }
    }

    pub fn first_text(&self) -> Option<&str> {
        self.content.iter().find_map(|b| match b {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
    }
}

/// 工具调用经过完整管线（含 `after_tool_call`）后的最终结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentToolCallOutcome {
    pub tool_call: ToolCall,
    pub result: AgentToolResult,
    pub is_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// 工具执行期间推送增量进度的回调类型。
pub type AgentToolUpdateCallback = Arc<dyn Fn(AgentToolResult) + Send + Sync>;

pub type PrepareArgumentsFn = Arc<dyn Fn(&JsonValue) -> JsonValue + Send + Sync>;

pub type ToolExecuteFn = Arc<
    dyn Fn(
            String,
            JsonValue,
            Option<CancellationToken>,
            Option<AgentToolUpdateCallback>,
        ) -> BoxFuture<'static, Result<AgentToolResult, String>>
        + Send
        + Sync,
>;

#[derive(Clone)]
struct ToolSchemaCache {
    parameters: JsonValue,
    validator: Arc<OnceLock<Arc<CompiledValidator>>>,
}

impl ToolSchemaCache {
    fn new(parameters: JsonValue) -> Self {
        Self {
            parameters,
            validator: Arc::new(OnceLock::new()),
        }
    }

    fn with_precompiled(parameters: JsonValue, validator: CompiledValidator) -> Self {
        let lock = OnceLock::new();
        let _ = lock.set(Arc::new(validator));
        Self {
            parameters,
            validator: Arc::new(lock),
        }
    }
}

/// Agent 可执行工具定义（对照 `AgentTool`）。
///
/// 工具的 `parameters` JSON Schema 与其预编译校验器 `CompiledValidator` 绑定为不可变单元，
/// 通过 `parameters()`、`set_parameters()`、`with_parameters()` 访问或替换，确保克隆或替换后绝不串用旧 Schema 的缓存。
#[derive(Clone)]
pub struct AgentTool {
    pub name: String,
    pub label: String,
    pub description: String,
    schema_cache: ToolSchemaCache,
    pub constrained_sampling: Option<ToolConstrainedSampling>,
    pub output_schema: Option<JsonValue>,
    pub replay: Option<ToolReplayPolicy>,
    pub execution_mode: Option<ToolExecutionMode>,
    pub prepare_arguments: Option<PrepareArgumentsFn>,
    pub execute: ToolExecuteFn,
}

impl std::fmt::Debug for AgentTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentTool")
            .field("name", &self.name)
            .field("label", &self.label)
            .field("description", &self.description)
            .field("parameters", &self.schema_cache.parameters)
            .field("execution_mode", &self.execution_mode)
            .finish()
    }
}

impl AgentTool {
    pub fn new(
        name: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
        parameters: JsonValue,
        execute: ToolExecuteFn,
    ) -> Self {
        Self {
            name: name.into(),
            label: label.into(),
            description: description.into(),
            schema_cache: ToolSchemaCache::new(parameters),
            constrained_sampling: None,
            output_schema: None,
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            execute,
        }
    }

    /// 构造工具并在注册阶段立即校验 Schema（若包含非法正则或无效结构则显式返回 `Err(SchemaCompileError)`）。
    pub fn try_new(
        name: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
        parameters: JsonValue,
        execute: ToolExecuteFn,
    ) -> Result<Self, SchemaCompileError> {
        let compiled = try_compile_schema(
            &parameters,
            CompileOptions {
                coerce: true,
                unresolved_refs: UnresolvedRefs::Reject,
                ..CompileOptions::default()
            },
        )?;
        Ok(Self {
            name: name.into(),
            label: label.into(),
            description: description.into(),
            schema_cache: ToolSchemaCache::with_precompiled(parameters, compiled),
            constrained_sampling: None,
            output_schema: None,
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            execute,
        })
    }

    /// 只读借用当前工具的参数 JSON Schema。
    pub fn parameters(&self) -> &JsonValue {
        &self.schema_cache.parameters
    }

    /// 替换当前工具实例的参数 JSON Schema，并为其分配全新的独立校验器缓存槽（不影响原工具或其他克隆体）。
    pub fn set_parameters(&mut self, parameters: JsonValue) {
        self.schema_cache = ToolSchemaCache::new(parameters);
    }

    /// 替换当前工具实例的参数 JSON Schema 并在编译期立即校验 Schema 合法性。
    pub fn try_set_parameters(&mut self, parameters: JsonValue) -> Result<(), SchemaCompileError> {
        let compiled = try_compile_schema(
            &parameters,
            CompileOptions {
                coerce: true,
                unresolved_refs: UnresolvedRefs::Reject,
                ..CompileOptions::default()
            },
        )?;
        self.schema_cache = ToolSchemaCache::with_precompiled(parameters, compiled);
        Ok(())
    }

    /// 链式替换参数 JSON Schema 并重置当前实例的校验器缓存。
    pub fn with_parameters(mut self, parameters: JsonValue) -> Self {
        self.set_parameters(parameters);
        self
    }

    /// 基于实现了 `DeserializeOwned + JsonSchema` 的强类型参数结构体构造工具。
    ///
    /// 注册时自动生成纯净的 JSON Schema；运行期先经过 `pi-schema` 三阶段宽松转换与预编译校验，
    /// 再反序列化为强类型 `P` 传入执行闭包。
    pub fn from_typed<P, F, Fut>(
        name: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
        execute: F,
    ) -> Self
    where
        P: DeserializeOwned + JsonSchema + Send + 'static,
        F: Fn(String, P, Option<CancellationToken>, Option<AgentToolUpdateCallback>) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: Future<Output = Result<AgentToolResult, String>> + Send + 'static,
    {
        let root_schema = schemars::schema_for!(P);
        let mut parameters = root_schema.to_value();
        normalize_schemars_node(&mut parameters);
        let exec_fn = Arc::new(execute);
        let wrapped: ToolExecuteFn =
            Arc::new(move |tool_call_id, coerced_args, cancel_token, on_update| {
                let exec_fn = exec_fn.clone();
                Box::pin(async move {
                    let typed_params: P = serde_json::from_value(coerced_args)
                        .map_err(|e| format!("Failed to deserialize tool arguments: {e}"))?;
                    exec_fn(tool_call_id, typed_params, cancel_token, on_update).await
                })
            });
        Self::new(name, label, description, parameters, wrapped)
    }

    /// 提取仅包含模型可见声明字段的 `pi_ai::Tool`（对照 `toToolDeclaration`）。
    pub fn to_tool_declaration(&self) -> Tool {
        Tool {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.schema_cache.parameters.clone(),
            constrained_sampling: self.constrained_sampling.clone(),
        }
    }

    /// 获取或惰性预编译当前工具的参数校验器（返回拥有型 `Arc<CompiledValidator>`）。
    pub fn compiled_validator(&self) -> Arc<CompiledValidator> {
        self.schema_cache
            .validator
            .get_or_init(|| Arc::new(compile_tool_schema(&self.schema_cache.parameters)))
            .clone()
    }
}

/// 将 `schemars` 生成的 `Option<T>` 属性（非 `required` 字段上的 `["T", "null"]` 或 `anyOf: [S, {"type": "null"}]`）
/// 归一化为与 `pi-schema` `opt(T)` 一致的纯净可选属性 Schema。
fn normalize_schemars_node(node: &mut Value) {
    let Value::Object(obj) = node else {
        return;
    };
    obj.remove("$schema");
    obj.remove("title");

    let required: BTreeSet<String> = obj
        .get("required")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_owned()))
                .collect()
        })
        .unwrap_or_default();

    if let Some(Value::Object(props)) = obj.get_mut("properties") {
        for (key, prop_schema) in props.iter_mut() {
            if !required.contains(key)
                && let Value::Object(p_obj) = prop_schema
            {
                if let Some(Value::Array(types)) = p_obj.get_mut("type") {
                    types.retain(|t| t.as_str() != Some("null"));
                    if types.len() == 1 {
                        let single = types[0].clone();
                        p_obj.insert("type".to_owned(), single);
                    }
                } else if let Some(Value::Array(any_of)) = p_obj.get_mut("anyOf") {
                    any_of.retain(|branch| {
                        branch
                            .as_object()
                            .and_then(|b| b.get("type"))
                            .and_then(|t| t.as_str())
                            != Some("null")
                    });
                    if any_of.len() == 1 {
                        let single = any_of.pop().expect("single branch");
                        if let Value::Object(single_obj) = single {
                            p_obj.remove("anyOf");
                            for (k, v) in single_obj {
                                p_obj.insert(k, v);
                            }
                        }
                    }
                }
            }
            normalize_schemars_node(prop_schema);
        }
    }

    for defs_key in ["$defs", "definitions"] {
        if let Some(Value::Object(defs)) = obj.get_mut(defs_key) {
            for def_schema in defs.values_mut() {
                normalize_schemars_node(def_schema);
            }
        }
    }
}

// =============================================================================
// 1.3 AgentEvent (snake_case type tag + camelCase fields)
// =============================================================================

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AgentEvent {
    AgentStart,
    AgentEnd {
        messages: Vec<AgentMessage>,
    },
    TurnStart,
    TurnEnd {
        message: AgentMessage,
        tool_results: Vec<ToolResultMessage>,
    },
    MessageStart {
        message: AgentMessage,
    },
    MessageUpdate {
        message: AgentMessage,
        assistant_message_event: AssistantMessageEvent,
    },
    MessageEnd {
        message: AgentMessage,
    },
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: JsonValue,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_tool_call_id: Option<String>,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        args: JsonValue,
        partial_result: AgentToolResult,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_tool_call_id: Option<String>,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: AgentToolResult,
        is_error: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_tool_call_id: Option<String>,
    },
}

// =============================================================================
// 1.4 Context, Hooks, StreamFn & Config Types
// =============================================================================

#[derive(Debug, Clone, Default)]
pub struct AgentContext {
    pub messages: Vec<AgentMessage>,
    pub tools: Vec<AgentTool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeToolCallResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<JsonValue>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AfterToolCallResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<UserContentBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct BeforeToolCallContext {
    pub assistant_message: AssistantMessage,
    pub tool_call: ToolCall,
    pub args: JsonValue,
    pub context: AgentContext,
}

#[derive(Debug, Clone)]
pub struct AfterToolCallContext {
    pub assistant_message: AssistantMessage,
    pub tool_call: ToolCall,
    pub args: JsonValue,
    pub result: AgentToolResult,
    pub is_error: bool,
    pub context: AgentContext,
}

#[derive(Debug, Clone)]
pub struct AgentTurnContext {
    pub message: AssistantMessage,
    pub tool_results: Vec<ToolResultMessage>,
    pub context: AgentContext,
    pub new_messages: Vec<AgentMessage>,
}

pub type PrepareNextTurnContext = AgentTurnContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum AgentTurnDecision {
    Continue,
    End,
}

#[derive(Debug, Clone, Default)]
pub struct AgentLoopTurnUpdate {
    pub context: Option<AgentContext>,
    pub messages: Option<Vec<AgentMessage>>,
    pub model: Option<Model>,
    pub thinking_level: Option<ModelThinkingLevel>,
}

#[derive(Debug, Clone)]
pub struct PrepareRequestContext {
    pub context: AgentContext,
    pub model: Model,
    pub thinking_level: ModelThinkingLevel,
}

#[derive(Debug, Clone, Default)]
pub struct AgentRequestUpdate {
    pub context: Option<AgentContext>,
    pub model: Option<Model>,
    pub thinking_level: Option<ModelThinkingLevel>,
}

/// 模型流式请求函数签名（对照 `StreamFn`）。
pub type StreamFn = Arc<
    dyn Fn(
            Model,
            TranscriptContext,
            SimpleStreamOptions,
        ) -> BoxFuture<'static, AssistantMessageEventStream>
        + Send
        + Sync,
>;

/// 将 `Arc<pi_ai::Models>` 适配为 `pi-agent` 的 `StreamFn`（调用 `models.stream_simple`）。
pub fn models_stream_fn(models: Arc<Models>) -> StreamFn {
    Arc::new(move |model, transcript, options| {
        let models = models.clone();
        Box::pin(async move {
            let ctx = pi_ai::Context {
                system_prompt: None,
                messages: transcript.into_messages(),
                tools: None,
            };
            models.stream_simple(&model, &ctx, Some(&options))
        })
    })
}

pub type ConvertToLlmFn =
    Arc<dyn Fn(Vec<AgentMessage>) -> BoxFuture<'static, Vec<Message>> + Send + Sync>;

pub type TransformContextFn = Arc<
    dyn Fn(Vec<AgentMessage>, Option<CancellationToken>) -> BoxFuture<'static, Vec<AgentMessage>>
        + Send
        + Sync,
>;

pub type GetApiKeyFn = Arc<dyn Fn(String) -> BoxFuture<'static, Option<String>> + Send + Sync>;

pub type FinishTurnFn = Arc<
    dyn Fn(
            AgentTurnContext,
            Option<CancellationToken>,
        ) -> BoxFuture<'static, Option<AgentTurnDecision>>
        + Send
        + Sync,
>;

pub type PrepareRequestFn = Arc<
    dyn Fn(
            PrepareRequestContext,
            Option<CancellationToken>,
        ) -> BoxFuture<'static, Option<AgentRequestUpdate>>
        + Send
        + Sync,
>;

pub type PrepareNextTurnFn = Arc<
    dyn Fn(PrepareNextTurnContext) -> BoxFuture<'static, Option<AgentLoopTurnUpdate>> + Send + Sync,
>;

pub type GetQueuedMessagesFn = Arc<dyn Fn() -> BoxFuture<'static, Vec<AgentMessage>> + Send + Sync>;

pub type BeforeToolCallFn = Arc<
    dyn for<'a> Fn(
            &'a mut BeforeToolCallContext,
            Option<CancellationToken>,
        ) -> BoxFuture<'a, Result<Option<BeforeToolCallResult>, String>>
        + Send
        + Sync,
>;

pub type AfterToolCallFn = Arc<
    dyn Fn(
            AfterToolCallContext,
            Option<CancellationToken>,
        ) -> BoxFuture<'static, Result<Option<AfterToolCallResult>, String>>
        + Send
        + Sync,
>;

/// 可失败的消息定稿钩子：所有输入、助手回复与工具结果在进入正式上下文和发出 `message_end` 前均执行一次
/// （工具结果在 `message_start` 前完成定稿）。
pub type FinalizeMessageFn = Arc<
    dyn Fn(
            AgentMessage,
            Option<CancellationToken>,
        ) -> BoxFuture<'static, Result<AgentMessage, String>>
        + Send
        + Sync,
>;

/// 无状态执行循环配置（对照 `AgentLoopConfig`）。
#[derive(Clone)]
pub struct AgentLoopConfig {
    pub model: Model,
    pub reasoning: Option<ThinkingLevel>,
    pub temperature: Option<f64>,
    pub sampling_params: Option<SamplingParams>,
    pub max_tokens: Option<u64>,
    pub api_key: Option<String>,
    pub headers: Option<ProviderHeaders>,
    pub transport: Option<Transport>,
    pub cache_retention: Option<CacheRetention>,
    pub session_id: Option<String>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub max_retry_delay_ms: Option<u64>,
    pub tool_choice: Option<ToolChoice>,
    pub identity: Option<ClientIdentity>,
    pub on_headers: Option<OnHeadersFn>,
    pub on_payload: Option<OnPayloadFn>,
    pub on_response: Option<OnResponseFn>,
    pub on_provider_stream_event: Option<OnProviderStreamEventFn>,
    pub convert_to_llm: ConvertToLlmFn,
    pub transform_context: Option<TransformContextFn>,
    pub get_api_key: Option<GetApiKeyFn>,
    pub finish_turn: Option<FinishTurnFn>,
    pub prepare_request: Option<PrepareRequestFn>,
    pub prepare_next_turn: Option<PrepareNextTurnFn>,
    pub get_steering_messages: Option<GetQueuedMessagesFn>,
    pub get_follow_up_messages: Option<GetQueuedMessagesFn>,
    pub tool_execution: ToolExecutionMode,
    pub before_tool_call: Option<BeforeToolCallFn>,
    pub after_tool_call: Option<AfterToolCallFn>,
    pub finalize_message: Option<FinalizeMessageFn>,
}

impl AgentLoopConfig {
    pub fn new(model: Model) -> Self {
        Self {
            model,
            reasoning: None,
            temperature: None,
            sampling_params: None,
            max_tokens: None,
            api_key: None,
            headers: None,
            transport: None,
            cache_retention: None,
            session_id: None,
            thinking_budgets: None,
            max_retry_delay_ms: None,
            tool_choice: None,
            identity: None,
            on_headers: None,
            on_payload: None,
            on_response: None,
            on_provider_stream_event: None,
            convert_to_llm: Arc::new(|messages| {
                Box::pin(async move { default_convert_to_llm(&messages) })
            }),
            transform_context: None,
            get_api_key: None,
            finish_turn: None,
            prepare_request: None,
            prepare_next_turn: None,
            get_steering_messages: None,
            get_follow_up_messages: None,
            tool_execution: ToolExecutionMode::Parallel,
            before_tool_call: None,
            after_tool_call: None,
            finalize_message: None,
        }
    }
}

/// Agent 状态快照（对照 `AgentState`）。
#[derive(Debug, Clone)]
pub struct AgentState {
    pub system_prompt: String,
    pub model: Model,
    pub thinking_level: ModelThinkingLevel,
    pub tools: Vec<AgentTool>,
    pub messages: Vec<AgentMessage>,
    pub is_streaming: bool,
    pub streaming_message: Option<AgentMessage>,
    pub pending_tool_calls: BTreeSet<String>,
    pub error_message: Option<String>,
}

pub fn default_agent_model() -> Model {
    Model {
        id: "unknown".to_owned(),
        name: "unknown".to_owned(),
        api: "unknown".to_owned(),
        provider: "unknown".to_owned(),
        base_url: String::new(),
        input: Vec::new(),
        input_limits: None,
        cost: pi_ai::ModelCost::default(),
        headers: None,
        model_type: None,
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: 0,
        max_tokens: 0,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    }
}

pub fn empty_usage() -> Usage {
    Usage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 0,
        cost: UsageCost::default(),
    }
}

/// 构造 `Agent` 时的初始状态配置（对照 `AgentInitialState`）。
#[derive(Debug, Clone, Default)]
pub struct AgentInitialState {
    pub system_prompt: Option<String>,
    pub model: Option<Model>,
    pub thinking_level: Option<ModelThinkingLevel>,
    pub tools: Option<Vec<AgentTool>>,
    pub messages: Option<Vec<AgentMessage>>,
}

/// 文本/消息形式的 `prompt()` 输入。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum PromptInput {
    Text {
        text: String,
        images: Vec<ImageContent>,
    },
    Single(AgentMessage),
    Batch(Vec<AgentMessage>),
}

impl From<&str> for PromptInput {
    fn from(s: &str) -> Self {
        PromptInput::Text {
            text: s.to_owned(),
            images: Vec::new(),
        }
    }
}

impl From<String> for PromptInput {
    fn from(s: String) -> Self {
        PromptInput::Text {
            text: s,
            images: Vec::new(),
        }
    }
}

impl From<AgentMessage> for PromptInput {
    fn from(m: AgentMessage) -> Self {
        PromptInput::Single(m)
    }
}

impl From<Vec<AgentMessage>> for PromptInput {
    fn from(m: Vec<AgentMessage>) -> Self {
        PromptInput::Batch(m)
    }
}

impl PromptInput {
    pub fn into_messages(self) -> Vec<AgentMessage> {
        match self {
            PromptInput::Batch(msgs) => msgs,
            PromptInput::Single(msg) => vec![msg],
            PromptInput::Text { text, images } => {
                let mut blocks = vec![UserContentBlock::Text(TextContent::new(text))];
                for img in images {
                    blocks.push(UserContentBlock::Image(img));
                }
                vec![AgentMessage::User(UserMessage {
                    content: pi_ai::UserContent::Blocks(blocks),
                    timestamp: now_ms(),
                })]
            }
        }
    }
}

pub fn create_error_assistant_message(
    model: &Model,
    error_message: impl Into<String>,
    aborted: bool,
) -> AssistantMessage {
    AssistantMessage {
        content: vec![pi_ai::ContentBlock::Text(TextContent::new(""))],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: empty_usage(),
        stop_reason: if aborted {
            StopReason::Aborted
        } else {
            StopReason::Error
        },
        deferred: None,
        error_message: Some(error_message.into()),
        raw_stop_reason: None,
        end_turn: None,
        timestamp: now_ms(),
        duration_ms: None,
    }
}

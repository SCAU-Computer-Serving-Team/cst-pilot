use std::sync::Arc;

use pi_agent::{AgentMessage, ConvertToLlmFn, CustomAgentMessage};
use pi_ai::{
    JsonObject, JsonValue, Message, TextContent, UserContent, UserContentBlock, UserMessage,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const COMPACTION_SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
pub const COMPACTION_SUMMARY_SUFFIX: &str = "\n</summary>";

pub const BRANCH_SUMMARY_PREFIX: &str =
    "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n";
pub const BRANCH_SUMMARY_SUFFIX: &str = "</summary>";

/// `!` / `!!` 命令执行的会话消息（对照 `BashExecutionMessage`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashExecutionMessage {
    pub command: String,
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
    pub timestamp: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_from_context: Option<bool>,
}

impl BashExecutionMessage {
    pub fn to_agent_message(&self) -> AgentMessage {
        let mut data = JsonObject::new();
        data.insert("command".to_owned(), Value::String(self.command.clone()));
        data.insert("output".to_owned(), Value::String(self.output.clone()));
        if let Some(code) = self.exit_code {
            data.insert("exitCode".to_owned(), json!(code));
        }
        data.insert("cancelled".to_owned(), Value::Bool(self.cancelled));
        data.insert("truncated".to_owned(), Value::Bool(self.truncated));
        if let Some(path) = &self.full_output_path {
            data.insert("fullOutputPath".to_owned(), Value::String(path.clone()));
        }
        if let Some(excl) = self.exclude_from_context {
            data.insert("excludeFromContext".to_owned(), Value::Bool(excl));
        }
        AgentMessage::Custom(CustomAgentMessage::new(
            "bashExecution",
            data,
            self.timestamp,
        ))
    }

    pub fn from_agent_message(msg: &AgentMessage) -> Option<Self> {
        let AgentMessage::Custom(c) = msg else {
            return None;
        };
        if c.role != "bashExecution" {
            return None;
        }
        Some(Self {
            command: c
                .data
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            output: c
                .data
                .get("output")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            exit_code: c
                .data
                .get("exitCode")
                .and_then(Value::as_i64)
                .map(|n| n as i32),
            cancelled: c
                .data
                .get("cancelled")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            truncated: c
                .data
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            full_output_path: c
                .data
                .get("fullOutputPath")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            timestamp: c.timestamp,
            exclude_from_context: c.data.get("excludeFromContext").and_then(Value::as_bool),
        })
    }
}

/// 扩展自定义消息内容（支持纯字符串或文本/图片内容块数组）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CustomMessageContent {
    Text(String),
    Blocks(Vec<UserContentBlock>),
}

impl Default for CustomMessageContent {
    fn default() -> Self {
        Self::Text(String::new())
    }
}

impl From<&str> for CustomMessageContent {
    fn from(s: &str) -> Self {
        Self::Text(s.to_owned())
    }
}

impl From<String> for CustomMessageContent {
    fn from(s: String) -> Self {
        Self::Text(s)
    }
}

impl From<Vec<UserContentBlock>> for CustomMessageContent {
    fn from(blocks: Vec<UserContentBlock>) -> Self {
        Self::Blocks(blocks)
    }
}

impl CustomMessageContent {
    pub fn to_blocks(&self) -> Vec<UserContentBlock> {
        match self {
            CustomMessageContent::Text(s) => vec![UserContentBlock::Text(TextContent::new(s))],
            CustomMessageContent::Blocks(blocks) => blocks.clone(),
        }
    }

    pub fn text_summary(&self) -> String {
        match self {
            CustomMessageContent::Text(s) => s.clone(),
            CustomMessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    UserContentBlock::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

/// 扩展注入的自定义对话消息（对照 `CustomMessage`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessage {
    pub custom_type: String,
    pub content: CustomMessageContent,
    pub display: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
    pub timestamp: u64,
}

impl CustomMessage {
    pub fn to_agent_message(&self) -> AgentMessage {
        let mut data = JsonObject::new();
        data.insert(
            "customType".to_owned(),
            Value::String(self.custom_type.clone()),
        );
        data.insert(
            "content".to_owned(),
            serde_json::to_value(&self.content).unwrap_or(Value::String(String::new())),
        );
        data.insert("display".to_owned(), Value::Bool(self.display));
        if let Some(details) = &self.details {
            data.insert("details".to_owned(), details.clone());
        }
        AgentMessage::Custom(CustomAgentMessage::new("custom", data, self.timestamp))
    }

    pub fn from_agent_message(msg: &AgentMessage) -> Option<Self> {
        let AgentMessage::Custom(c) = msg else {
            return None;
        };
        if c.role != "custom" {
            return None;
        }
        let content = match c.data.get("content") {
            Some(Value::String(s)) => CustomMessageContent::Text(s.clone()),
            Some(Value::Array(arr)) => serde_json::from_value(Value::Array(arr.clone()))
                .map(CustomMessageContent::Blocks)
                .unwrap_or_else(|_| CustomMessageContent::Text(String::new())),
            _ => CustomMessageContent::Text(String::new()),
        };
        Some(Self {
            custom_type: c
                .data
                .get("customType")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            content,
            display: c
                .data
                .get("display")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            details: c.data.get("details").cloned(),
            timestamp: c.timestamp,
        })
    }
}

/// 分支跳转时生成的分支总结消息（对照 `BranchSummaryMessage`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryMessage {
    pub summary: String,
    pub from_id: Option<String>,
    pub timestamp: u64,
}

impl BranchSummaryMessage {
    pub fn to_agent_message(&self) -> AgentMessage {
        let mut data = JsonObject::new();
        data.insert("summary".to_owned(), Value::String(self.summary.clone()));
        data.insert(
            "fromId".to_owned(),
            match &self.from_id {
                Some(id) => Value::String(id.clone()),
                None => Value::Null,
            },
        );
        AgentMessage::Custom(CustomAgentMessage::new(
            "branchSummary",
            data,
            self.timestamp,
        ))
    }

    pub fn from_agent_message(msg: &AgentMessage) -> Option<Self> {
        let AgentMessage::Custom(c) = msg else {
            return None;
        };
        if c.role != "branchSummary" {
            return None;
        }
        Some(Self {
            summary: c
                .data
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            from_id: c
                .data
                .get("fromId")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            timestamp: c.timestamp,
        })
    }
}

/// 上下文压缩生成的总结消息（对照 `CompactionSummaryMessage`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSummaryMessage {
    pub summary: String,
    pub tokens_before: u64,
    pub timestamp: u64,
}

impl CompactionSummaryMessage {
    pub fn to_agent_message(&self) -> AgentMessage {
        let mut data = JsonObject::new();
        data.insert("summary".to_owned(), Value::String(self.summary.clone()));
        data.insert("tokensBefore".to_owned(), json!(self.tokens_before));
        AgentMessage::Custom(CustomAgentMessage::new(
            "compactionSummary",
            data,
            self.timestamp,
        ))
    }

    pub fn from_agent_message(msg: &AgentMessage) -> Option<Self> {
        let AgentMessage::Custom(c) = msg else {
            return None;
        };
        if c.role != "compactionSummary" {
            return None;
        }
        Some(Self {
            summary: c
                .data
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            tokens_before: c
                .data
                .get("tokensBefore")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            timestamp: c.timestamp,
        })
    }
}

/// 将 `BashExecutionMessage` 格式化为发往 LLM 的用户消息文本（对照 `bashExecutionToText`）。
pub fn bash_execution_to_text(msg: &BashExecutionMessage) -> String {
    let mut text = format!("Ran `{}`\n", msg.command);
    if !msg.output.is_empty() {
        text.push_str(&format!("```\n{}\n```", msg.output));
    } else {
        text.push_str("(no output)");
    }
    if msg.cancelled {
        text.push_str("\n\n(command cancelled)");
    } else if let Some(code) = msg.exit_code
        && code != 0
    {
        text.push_str(&format!("\n\nCommand exited with code {code}"));
    }
    if msg.truncated
        && let Some(path) = &msg.full_output_path
        && !path.is_empty()
    {
        text.push_str(&format!("\n\n[Output truncated. Full output: {path}]"));
    }
    text
}

/// 将包含四类会话专属消息的 `AgentMessage` 序列转换为标准 LLM `Message` 序列（对照 `convertToLlm`）。
pub fn convert_to_llm(messages: &[AgentMessage]) -> Vec<Message> {
    let mut out = Vec::with_capacity(messages.len());
    for m in messages {
        match m {
            AgentMessage::System(s) => out.push(Message::System(s.clone())),
            AgentMessage::User(u) => out.push(Message::User(u.clone())),
            AgentMessage::Assistant(a) => out.push(Message::Assistant(a.clone())),
            AgentMessage::ToolResult(tr) => out.push(Message::ToolResult(tr.clone())),
            AgentMessage::Custom(c) => match c.role.as_str() {
                "bashExecution" => {
                    if let Some(bash_msg) = BashExecutionMessage::from_agent_message(m) {
                        if bash_msg.exclude_from_context == Some(true) {
                            continue;
                        }
                        out.push(Message::User(UserMessage {
                            content: UserContent::Blocks(vec![UserContentBlock::Text(
                                TextContent::new(bash_execution_to_text(&bash_msg)),
                            )]),
                            timestamp: bash_msg.timestamp,
                        }));
                    }
                }
                "custom" => {
                    if let Some(custom_msg) = CustomMessage::from_agent_message(m) {
                        out.push(Message::User(UserMessage {
                            content: UserContent::Blocks(custom_msg.content.to_blocks()),
                            timestamp: custom_msg.timestamp,
                        }));
                    }
                }
                "branchSummary" => {
                    if let Some(branch_msg) = BranchSummaryMessage::from_agent_message(m) {
                        let text = format!(
                            "{BRANCH_SUMMARY_PREFIX}{}{BRANCH_SUMMARY_SUFFIX}",
                            branch_msg.summary
                        );
                        out.push(Message::User(UserMessage {
                            content: UserContent::Blocks(vec![UserContentBlock::Text(
                                TextContent::new(text),
                            )]),
                            timestamp: branch_msg.timestamp,
                        }));
                    }
                }
                "compactionSummary" => {
                    if let Some(compact_msg) = CompactionSummaryMessage::from_agent_message(m) {
                        let text = format!(
                            "{COMPACTION_SUMMARY_PREFIX}{}{COMPACTION_SUMMARY_SUFFIX}",
                            compact_msg.summary
                        );
                        out.push(Message::User(UserMessage {
                            content: UserContent::Blocks(vec![UserContentBlock::Text(
                                TextContent::new(text),
                            )]),
                            timestamp: compact_msg.timestamp,
                        }));
                    }
                }
                _ => {}
            },
        }
    }
    out
}

/// 构造供 `Agent` / `AgentLoopConfig` 使用的异步 `ConvertToLlmFn`。
pub fn session_convert_to_llm_fn() -> ConvertToLlmFn {
    Arc::new(|messages| Box::pin(async move { convert_to_llm(&messages) }))
}

pub const BLOCKED_IMAGE_PLACEHOLDER: &str = "Image reading is disabled.";

fn filter_user_content_blocks(blocks: Vec<UserContentBlock>) -> Vec<UserContentBlock> {
    if !blocks
        .iter()
        .any(|b| matches!(b, UserContentBlock::Image(_)))
    {
        return blocks;
    }
    let mut out: Vec<UserContentBlock> = Vec::with_capacity(blocks.len());
    for block in blocks {
        match block {
            UserContentBlock::Image(_) => {
                let is_dup = matches!(
                    out.last(),
                    Some(UserContentBlock::Text(prev)) if prev.text == BLOCKED_IMAGE_PLACEHOLDER
                );
                if !is_dup {
                    out.push(UserContentBlock::Text(TextContent::new(
                        BLOCKED_IMAGE_PLACEHOLDER,
                    )));
                }
            }
            UserContentBlock::Text(t) => {
                if t.text == BLOCKED_IMAGE_PLACEHOLDER
                    && matches!(
                        out.last(),
                        Some(UserContentBlock::Text(prev)) if prev.text == BLOCKED_IMAGE_PLACEHOLDER
                    )
                {
                    continue;
                }
                out.push(UserContentBlock::Text(t));
            }
        }
    }
    out
}

/// 当开启 `block_images` 时，将所有用户消息与工具结果中的图片块替换为占位文本并折叠连续重复项。
pub fn filter_blocked_images(messages: Vec<Message>) -> Vec<Message> {
    messages
        .into_iter()
        .map(|msg| match msg {
            Message::User(mut u) => {
                if let UserContent::Blocks(blocks) = u.content {
                    u.content = UserContent::Blocks(filter_user_content_blocks(blocks));
                }
                Message::User(u)
            }
            Message::ToolResult(mut tr) => {
                tr.content = filter_user_content_blocks(tr.content);
                Message::ToolResult(tr)
            }
            other => other,
        })
        .collect()
}

/// 支持动态 `block_images` 过滤的会话消息转 LLM 消息入口。
pub fn convert_to_llm_with_options(messages: &[AgentMessage], block_images: bool) -> Vec<Message> {
    let converted = convert_to_llm(messages);
    if block_images {
        filter_blocked_images(converted)
    } else {
        converted
    }
}

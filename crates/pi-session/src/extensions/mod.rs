use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use futures_util::FutureExt;

use pi_agent::{
    AgentMessage, AgentTool, AgentToolCallOutcome, AgentToolResult, AgentToolUpdateCallback,
    BoxFuture, PrepareArgumentsFn, ToolExecutionMode, ToolReplayPolicy,
    get_agent_current_system_message,
};
use pi_ai::{
    AssistantMessageEvent, ImageContent, JsonValue, Model, ModelThinkingLevel, Models, Provider,
    ProviderHeaders, ToolConstrainedSampling, Usage, UserContentBlock, create_provider,
};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::bash::{BashExecutor, BashResult};
use crate::branch_summary::BranchSummaryResult;
use crate::compaction::{CompactionPreparation, CompactionResult};
use crate::event_bus::EventBus;
use crate::extension_catalog::{CatalogChange, ExtensionCatalog};

/// Native provider configuration; `register_provider_config` sets its ID from the name argument.
pub type ProviderConfig = pi_ai::CreateProviderOptions;
use crate::mcp_servers::{
    McpServerConfig, RegisteredMcpServer, mcp_namespace, validate_mcp_server_config,
};
use crate::messages::CustomMessageContent;
use crate::nested_calls::NestedToolCallOptions;
use crate::session_manager::{
    CompactionEntry, SessionBoundaryDraft, SessionEntry, SessionTreeNode,
};
use crate::slash_commands::{SlashCommandInfo, SourceInfo};
use crate::system_prompt::{
    BuildSystemPromptOptions, NormalizedBuildSystemPromptOptions, build_system_prompt,
    normalize_build_system_prompt_options,
};
use crate::ui::{
    ExtensionMode, ExtensionUiContext, NoOpUiContext, UiDialogOptions, UiNotifyLevel, UiPromptKind,
    WorkingIndicatorOptions,
};
use crate::virtual_models::VirtualModelDefinition;

async fn isolate_handler<T>(
    future: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    std::panic::AssertUnwindSafe(future)
        .catch_unwind()
        .await
        .unwrap_or_else(|panic| {
            let message = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("unknown panic");
            Err(format!("Extension handler panicked: {message}"))
        })
}

pub const RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS: &[&str] = &[
    "app.interrupt",
    "app.clear",
    "app.exit",
    "app.suspend",
    "app.thinking.cycle",
    "app.model.cycleForward",
    "app.model.cycleBackward",
    "app.model.select",
    "app.tools.expand",
    "app.thinking.toggle",
    "app.editor.external",
    "app.message.copy",
    "app.message.followUp",
    "tui.input.submit",
    "tui.select.confirm",
    "tui.select.cancel",
    "tui.input.copy",
    "tui.editor.deleteToLineEnd",
];

pub const DEFAULT_STALE_CONTEXT_MESSAGE: &str = "This extension ctx is stale after session replacement or reload. Do not use a captured pi or command ctx after ctx.new_session(), ctx.fork(), ctx.switch_session(), or ctx.reload().";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("Extension initialization error in {extension_path}: {message}")]
pub struct ExtensionInitError {
    pub extension_path: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionError {
    pub extension_path: String,
    pub event: String,
    pub error: String,
}

pub type ExtensionErrorListener = Arc<dyn Fn(&ExtensionError) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceDiagnostic {
    pub kind: String,
    pub message: String,
    pub path: String,
}

mod context;
mod events;
mod runner;
mod tools;

pub use context::*;
pub use events::*;
pub use runner::*;
pub use tools::*;

use context::PromptTrackingUiContext;
use runner::ExtensionRunnerInner;

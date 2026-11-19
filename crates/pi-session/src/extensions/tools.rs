use super::*;

// =============================================================================
// 1. ToolExposure, ToolLoadout, ToolDefinition & Tool Context
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolExposure {
    #[default]
    Direct,
    ModelOnly,
    Codemode,
    Deferred,
    Hidden,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolNamespace {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAnnotations {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct ToolLoadout {
    pub all_tools: Vec<ToolDefinition>,
    pub active_tool_names: Vec<String>,
    pub declared_tools: Vec<AgentTool>,
}

impl ToolLoadout {
    pub fn get_exposure(&self, name: &str) -> ToolExposure {
        self.all_tools
            .iter()
            .find(|t| t.name == name)
            .map_or(ToolExposure::Direct, |t| t.exposure)
    }

    pub fn get_namespace(&self, name: &str) -> Option<&ToolNamespace> {
        self.all_tools
            .iter()
            .find(|t| t.name == name)
            .and_then(|t| t.namespace.as_ref())
    }

    pub fn get_prompt_guidelines(&self, name: &str) -> &[String] {
        self.all_tools
            .iter()
            .find(|t| t.name == name)
            .and_then(|t| t.prompt_guidelines.as_deref())
            .unwrap_or(&[])
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolLoadoutChanges {
    pub descriptions: BTreeMap<String, String>,
    pub hidden_declarations: Vec<String>,
}

pub type PrepareLoadoutFn = Arc<dyn Fn(&ToolLoadout) -> Option<ToolLoadoutChanges> + Send + Sync>;

pub type ExtensionToolExecuteFn = Arc<
    dyn Fn(
            String,
            JsonValue,
            Option<CancellationToken>,
            Option<AgentToolUpdateCallback>,
            ExtensionToolContext,
        ) -> BoxFuture<'static, Result<AgentToolResult, String>>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub label: String,
    pub description: String,
    pub prompt_snippet: Option<String>,
    pub prompt_guidelines: Option<Vec<String>>,
    pub parameters: JsonValue,
    pub output_schema: Option<JsonValue>,
    pub constrained_sampling: Option<ToolConstrainedSampling>,
    pub exposure: ToolExposure,
    pub default_active: Option<bool>,
    pub namespace: Option<ToolNamespace>,
    pub annotations: Option<ToolAnnotations>,
    pub prepare_arguments: Option<PrepareArgumentsFn>,
    pub prepare_loadout: Option<PrepareLoadoutFn>,
    pub execution_mode: Option<ToolExecutionMode>,
    /// 重放策略（默认 `ToolReplayPolicy::Never`）。
    pub replay_policy: ToolReplayPolicy,
    pub execute: ExtensionToolExecuteFn,
}

impl std::fmt::Debug for ToolDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolDefinition")
            .field("name", &self.name)
            .field("label", &self.label)
            .field("exposure", &self.exposure)
            .field("replay_policy", &self.replay_policy)
            .finish()
    }
}

impl ToolDefinition {
    pub fn new(
        name: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
        parameters: JsonValue,
        execute: ExtensionToolExecuteFn,
    ) -> Self {
        Self {
            name: name.into(),
            label: label.into(),
            description: description.into(),
            prompt_snippet: None,
            prompt_guidelines: None,
            parameters,
            output_schema: None,
            constrained_sampling: None,
            exposure: ToolExposure::Direct,
            default_active: None,
            namespace: None,
            annotations: None,
            prepare_arguments: None,
            prepare_loadout: None,
            execution_mode: None,
            replay_policy: ToolReplayPolicy::Never,
            execute,
        }
    }

    pub fn from_typed<P, F, Fut>(
        name: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
        execute: F,
    ) -> Self
    where
        P: DeserializeOwned + JsonSchema + Send + 'static,
        F: Fn(
                String,
                P,
                Option<CancellationToken>,
                Option<AgentToolUpdateCallback>,
                ExtensionToolContext,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = Result<AgentToolResult, String>> + Send + 'static,
    {
        let root_schema = schemars::schema_for!(P);
        let parameters = root_schema.to_value();
        let exec_fn = Arc::new(execute);
        let wrapped: ExtensionToolExecuteFn = Arc::new(
            move |tool_call_id, coerced_args, cancel_token, on_update, ctx| {
                let exec_fn = exec_fn.clone();
                Box::pin(async move {
                    let typed_params: P = serde_json::from_value(coerced_args)
                        .map_err(|e| format!("Failed to deserialize tool arguments: {e}"))?;
                    exec_fn(tool_call_id, typed_params, cancel_token, on_update, ctx).await
                })
            },
        );
        Self::new(name, label, description, parameters, wrapped)
    }

    pub fn new_typed_with_context<P, F, Fut>(
        name: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
        execute: F,
    ) -> Self
    where
        P: DeserializeOwned + JsonSchema + Send + 'static,
        F: Fn(
                String,
                P,
                Option<AgentToolUpdateCallback>,
                CancellationToken,
                ExtensionToolContext,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = Result<AgentToolResult, String>> + Send + 'static,
    {
        let exec = Arc::new(execute);
        Self::from_typed(
            name,
            label,
            description,
            move |id, params, cancel, on_update, ctx| {
                let exec = exec.clone();
                async move { exec(id, params, on_update, cancel.unwrap_or_default(), ctx).await }
            },
        )
    }

    pub fn from_agent_tool(tool: AgentTool) -> Self {
        let exec = tool.execute.clone();
        let replay_policy = tool.replay.unwrap_or(ToolReplayPolicy::Never);
        Self {
            name: tool.name.clone(),
            label: tool.label.clone(),
            description: tool.description.clone(),
            prompt_snippet: None,
            prompt_guidelines: None,
            parameters: tool.parameters().clone(),
            output_schema: tool.output_schema.clone(),
            constrained_sampling: tool.constrained_sampling.clone(),
            exposure: ToolExposure::Direct,
            default_active: None,
            namespace: None,
            annotations: None,
            prepare_arguments: tool.prepare_arguments.clone(),
            prepare_loadout: None,
            execution_mode: tool.execution_mode,
            replay_policy,
            execute: Arc::new(move |id, args, cancel, on_update, _ctx| {
                (exec)(id, args, cancel, on_update)
            }),
        }
    }

    pub fn with_prompt_snippet(mut self, snippet: impl Into<String>) -> Self {
        self.prompt_snippet = Some(snippet.into());
        self
    }

    pub fn with_prompt_guidelines(mut self, guidelines: Vec<String>) -> Self {
        self.prompt_guidelines = Some(guidelines);
        self
    }

    pub fn with_prepare_arguments(mut self, prepare: PrepareArgumentsFn) -> Self {
        self.prepare_arguments = Some(prepare);
        self
    }

    pub fn with_exposure(mut self, exposure: ToolExposure) -> Self {
        self.exposure = exposure;
        self
    }

    pub fn with_default_active(mut self, default_active: bool) -> Self {
        self.default_active = Some(default_active);
        self
    }

    pub fn with_namespace(mut self, namespace: ToolNamespace) -> Self {
        self.namespace = Some(namespace);
        self
    }

    pub fn with_annotations(mut self, annotations: ToolAnnotations) -> Self {
        self.annotations = Some(annotations);
        self
    }

    pub fn with_output_schema(mut self, output_schema: JsonValue) -> Self {
        self.output_schema = Some(output_schema);
        self
    }

    pub fn with_prepare_loadout(mut self, prepare_loadout: PrepareLoadoutFn) -> Self {
        self.prepare_loadout = Some(prepare_loadout);
        self
    }

    pub fn with_replay_policy(mut self, policy: ToolReplayPolicy) -> Self {
        self.replay_policy = policy;
        self
    }

    pub fn is_default_active(&self) -> bool {
        if self.exposure == ToolExposure::Hidden {
            return false;
        }
        self.default_active.unwrap_or(matches!(
            self.exposure,
            ToolExposure::Direct | ToolExposure::ModelOnly
        ))
    }

    pub fn is_activatable(&self) -> bool {
        self.exposure != ToolExposure::Hidden
    }

    pub fn is_nested_callable(&self) -> bool {
        matches!(
            self.exposure,
            ToolExposure::Direct | ToolExposure::Codemode | ToolExposure::Deferred
        )
    }

    pub fn to_agent_tool<F>(&self, ctx_factory: F) -> AgentTool
    where
        F: Fn(String, Option<CancellationToken>) -> ExtensionToolContext + Send + Sync + 'static,
    {
        let exec = self.execute.clone();
        let factory = Arc::new(ctx_factory);
        let mut agent_tool = AgentTool::new(
            self.name.clone(),
            self.label.clone(),
            self.description.clone(),
            self.parameters.clone(),
            Arc::new(move |tool_call_id, args, cancel, on_update| {
                let ctx = (factory)(tool_call_id.clone(), cancel.clone());
                (exec)(tool_call_id, args, cancel, on_update, ctx)
            }),
        );
        agent_tool.output_schema = self.output_schema.clone();
        agent_tool.constrained_sampling = self.constrained_sampling.clone();
        agent_tool.prepare_arguments = self.prepare_arguments.clone();
        agent_tool.execution_mode = self.execution_mode;
        agent_tool.replay = Some(self.replay_policy);
        agent_tool
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub parameters: JsonValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_guidelines: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<ToolNamespace>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
    pub source_info: SourceInfo,
    pub exposure: ToolExposure,
    pub replay_policy: ToolReplayPolicy,
}

#[derive(Clone)]
pub struct RegisteredTool {
    pub definition: ToolDefinition,
    pub source_info: SourceInfo,
}

// =============================================================================
// 2. Commands, Flags & Shortcuts
// =============================================================================

pub type CommandHandlerFn = Arc<
    dyn Fn(String, ExtensionCommandContext) -> BoxFuture<'static, Result<(), String>> + Send + Sync,
>;

#[derive(Clone)]
pub struct RegisteredCommand {
    pub name: String,
    pub description: Option<String>,
    pub argument_hint: Option<String>,
    pub source_info: SourceInfo,
    pub handler: CommandHandlerFn,
}

#[derive(Clone)]
pub struct ResolvedCommand {
    pub command: RegisteredCommand,
    pub invocation_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtensionFlagType {
    Boolean,
    String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ExtensionFlagValue {
    Bool(bool),
    String(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionFlag {
    pub name: String,
    pub description: Option<String>,
    pub flag_type: ExtensionFlagType,
    pub default_value: Option<ExtensionFlagValue>,
    pub extension_path: String,
}

pub type ShortcutHandlerFn = Arc<dyn Fn(ExtensionContext) -> BoxFuture<'static, ()> + Send + Sync>;

#[derive(Clone)]
pub struct ExtensionShortcut {
    pub key: String,
    pub description: Option<String>,
    pub extension_path: String,
    pub handler: ShortcutHandlerFn,
}

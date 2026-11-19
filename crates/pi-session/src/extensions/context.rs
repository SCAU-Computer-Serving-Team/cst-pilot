use super::*;

// =============================================================================
// 4. ContextUsage, ExtensionContext, ExtensionToolContext & ExtensionCommandContext
// =============================================================================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    pub tokens: Option<u64>,
    pub context_window: u64,
    pub percent: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct CompactOptions {
    pub custom_instructions: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SendMessageOptions {
    pub trigger_turn: bool,
    pub deliver_as: Option<StreamingInputBehavior>,
}

#[derive(Debug, Clone, Default)]
pub struct NavigateTreeOptions {
    pub summarize: bool,
    pub custom_instructions: Option<String>,
    pub replace_instructions: bool,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ForkSessionOptions {
    pub position: Option<String>,
}

pub type SendMessageActionFn = Arc<
    dyn Fn(BeforeAgentStartCustomMessageDraft, SendMessageOptions) -> Result<(), String>
        + Send
        + Sync,
>;
pub type SendUserMessageActionFn = Arc<
    dyn Fn(String, Option<Vec<ImageContent>>, Option<StreamingInputBehavior>) -> Result<(), String>
        + Send
        + Sync,
>;
pub type AppendEntryActionFn =
    Arc<dyn Fn(String, Option<JsonValue>) -> Result<String, String> + Send + Sync>;
pub type SetSessionNameActionFn = Arc<dyn Fn(String) -> Result<String, String> + Send + Sync>;
pub type GetSessionNameActionFn = Arc<dyn Fn() -> Option<String> + Send + Sync>;
pub type SetLabelActionFn =
    Arc<dyn Fn(String, Option<String>) -> Result<String, String> + Send + Sync>;
pub type GetActiveToolsActionFn = Arc<dyn Fn() -> Vec<String> + Send + Sync>;
pub type GetAllToolsActionFn = Arc<dyn Fn() -> Vec<ToolInfo> + Send + Sync>;
pub type SetActiveToolsActionFn = Arc<dyn Fn(Vec<String>) -> Result<(), String> + Send + Sync>;
pub type RefreshToolsActionFn = Arc<dyn Fn() + Send + Sync>;
pub type GetCommandsActionFn = Arc<dyn Fn() -> Vec<SlashCommandInfo> + Send + Sync>;
pub type SetModelActionFn =
    Arc<dyn Fn(Model) -> BoxFuture<'static, Result<bool, String>> + Send + Sync>;
pub type GetThinkingLevelActionFn = Arc<dyn Fn() -> ModelThinkingLevel + Send + Sync>;
pub type SetThinkingLevelActionFn = Arc<dyn Fn(ModelThinkingLevel) + Send + Sync>;

pub type ExecuteNestedToolActionFn = Arc<
    dyn Fn(
            String,
            String,
            JsonValue,
            NestedToolCallOptions,
        ) -> BoxFuture<'static, AgentToolCallOutcome>
        + Send
        + Sync,
>;

pub type NavigateTreeActionFn = Arc<
    dyn Fn(String, NavigateTreeOptions) -> BoxFuture<'static, Result<bool, String>> + Send + Sync,
>;
pub type ForkSessionActionFn = Arc<
    dyn Fn(String, ForkSessionOptions) -> BoxFuture<'static, Result<bool, String>> + Send + Sync,
>;
pub type NewSessionActionFn =
    Arc<dyn Fn(Option<String>) -> BoxFuture<'static, Result<bool, String>> + Send + Sync>;
pub type SwitchSessionActionFn =
    Arc<dyn Fn(PathBuf) -> BoxFuture<'static, Result<bool, String>> + Send + Sync>;
pub type WaitForIdleActionFn = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;
pub type ReloadSessionActionFn =
    Arc<dyn Fn() -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesDiscoverEvent {
    pub cwd: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesDiscoverResult {
    #[serde(default)]
    pub skill_paths: Vec<String>,
    #[serde(default)]
    pub prompt_paths: Vec<String>,
    #[serde(default)]
    pub theme_paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheWarmingAction {
    Warm,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheWarmingDecision {
    pub phase: String,
    pub warm_cost: f64,
    pub miss_cost: f64,
    pub continuation_probability: f64,
    pub expected_savings: f64,
    pub economics_available: bool,
    pub action: CacheWarmingAction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheWarmingDecisionEvent {
    pub warm_cost: f64,
    pub miss_cost: f64,
    pub continuation_probability: f64,
    pub action: CacheWarmingAction,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheWarmingDecisionEventResult {
    pub action: Option<CacheWarmingAction>,
}

/// 会话替换完成后供 `with_session` 回调使用的受控上下文（对照 `ReplacedSessionContext`）。
#[derive(Clone)]
pub struct ReplacedSessionContext {
    base: ExtensionContext,
}

impl std::ops::Deref for ReplacedSessionContext {
    type Target = ExtensionContext;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl ReplacedSessionContext {
    pub fn new(base: ExtensionContext) -> Self {
        Self { base }
    }
}

#[derive(Clone)]
pub struct ExtensionActions {
    pub send_message: SendMessageActionFn,
    pub send_user_message: SendUserMessageActionFn,
    pub append_entry: AppendEntryActionFn,
    pub set_session_name: SetSessionNameActionFn,
    pub get_session_name: GetSessionNameActionFn,
    pub set_label: SetLabelActionFn,
    pub get_active_tools: GetActiveToolsActionFn,
    pub get_requested_tools: GetActiveToolsActionFn,
    pub get_all_tools: GetAllToolsActionFn,
    pub set_active_tools: SetActiveToolsActionFn,
    pub refresh_tools: RefreshToolsActionFn,
    pub get_commands: GetCommandsActionFn,
    pub set_model: SetModelActionFn,
    pub get_thinking_level: GetThinkingLevelActionFn,
    pub set_thinking_level: SetThinkingLevelActionFn,
}

#[derive(Clone)]
pub struct ExtensionContextActions {
    pub get_model: Arc<dyn Fn() -> Option<Model> + Send + Sync>,
    pub is_idle: Arc<dyn Fn() -> bool + Send + Sync>,
    pub get_cancel_token: Arc<dyn Fn() -> Option<CancellationToken> + Send + Sync>,
    pub abort: Arc<dyn Fn() + Send + Sync>,
    pub has_pending_messages: Arc<dyn Fn() -> bool + Send + Sync>,
    pub shutdown: Arc<dyn Fn() + Send + Sync>,
    pub get_context_usage: Arc<dyn Fn() -> Option<ContextUsage> + Send + Sync>,
    pub compact: Arc<dyn Fn(Option<CompactOptions>) + Send + Sync>,
    pub get_system_prompt: Arc<dyn Fn() -> String + Send + Sync>,
    pub get_system_prompt_options: Arc<dyn Fn() -> BuildSystemPromptOptions + Send + Sync>,
    pub get_callable_tools: Arc<dyn Fn() -> Vec<AgentTool> + Send + Sync>,
    pub execute_tool: Option<ExecuteNestedToolActionFn>,
    pub get_session_tree: Arc<dyn Fn() -> Vec<SessionTreeNode> + Send + Sync>,
    pub get_session_branch: Arc<dyn Fn() -> Vec<SessionEntry> + Send + Sync>,
    pub get_session_leaf_id: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    pub get_session_id: Arc<dyn Fn() -> String + Send + Sync>,
    pub get_session_file: Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>,
}

#[derive(Clone)]
pub struct ExtensionCommandContextActions {
    pub wait_for_idle: WaitForIdleActionFn,
    pub new_session: NewSessionActionFn,
    pub fork: ForkSessionActionFn,
    pub navigate_tree: NavigateTreeActionFn,
    pub switch_session: SwitchSessionActionFn,
    pub reload: Option<ReloadSessionActionFn>,
}

/// 包装 `ExtensionUiContext` 以在阻塞弹窗前后自动维护 `ui_prompt_depth` 并发出 `ui_prompt_start/end` 事件。
pub(super) struct PromptTrackingUiContext {
    pub(super) inner: Arc<dyn ExtensionUiContext>,
    pub(super) runner: Arc<ExtensionRunnerInner>,
}

impl ExtensionUiContext for PromptTrackingUiContext {
    fn has_ui(&self) -> bool {
        self.runner.assert_active().is_ok() && self.inner.has_ui()
    }

    fn select<'a>(
        &'a self,
        title: &'a str,
        options: &'a [String],
        opts: Option<UiDialogOptions>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Option<String>> {
        let runner = self.runner.clone();
        let inner = self.inner.clone();
        let title_opt = Some(title.to_owned());
        Box::pin(async move {
            if runner.assert_active().is_err() {
                return None;
            }
            runner
                .with_ui_prompt(
                    UiPromptKind::Select,
                    title_opt,
                    inner.select(title, options, opts, cancel),
                )
                .await
        })
    }

    fn confirm<'a>(
        &'a self,
        title: &'a str,
        message: &'a str,
        opts: Option<UiDialogOptions>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, bool> {
        let runner = self.runner.clone();
        let inner = self.inner.clone();
        let title_opt = Some(title.to_owned());
        Box::pin(async move {
            if runner.assert_active().is_err() {
                return false;
            }
            runner
                .with_ui_prompt(
                    UiPromptKind::Confirm,
                    title_opt,
                    inner.confirm(title, message, opts, cancel),
                )
                .await
        })
    }

    fn input<'a>(
        &'a self,
        title: &'a str,
        placeholder: Option<&'a str>,
        opts: Option<UiDialogOptions>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Option<String>> {
        let runner = self.runner.clone();
        let inner = self.inner.clone();
        let title_opt = Some(title.to_owned());
        Box::pin(async move {
            if runner.assert_active().is_err() {
                return None;
            }
            runner
                .with_ui_prompt(
                    UiPromptKind::Input,
                    title_opt,
                    inner.input(title, placeholder, opts, cancel),
                )
                .await
        })
    }

    fn editor<'a>(
        &'a self,
        title: &'a str,
        prefill: Option<&'a str>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Option<String>> {
        let runner = self.runner.clone();
        let inner = self.inner.clone();
        let title_opt = Some(title.to_owned());
        Box::pin(async move {
            if runner.assert_active().is_err() {
                return None;
            }
            runner
                .with_ui_prompt(
                    UiPromptKind::Editor,
                    title_opt,
                    inner.editor(title, prefill, cancel),
                )
                .await
        })
    }

    fn custom<'a>(
        &'a self,
        payload: JsonValue,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Option<JsonValue>> {
        let runner = self.runner.clone();
        let inner = self.inner.clone();
        Box::pin(async move {
            if runner.assert_active().is_err() {
                return None;
            }
            runner
                .with_ui_prompt(UiPromptKind::Custom, None, inner.custom(payload, cancel))
                .await
        })
    }

    fn notify(&self, message: &str, level: UiNotifyLevel) {
        if self.runner.assert_active().is_ok() {
            self.inner.notify(message, level);
        }
    }

    fn set_status(&self, key: &str, text: Option<&str>) {
        if self.runner.assert_active().is_ok() {
            self.inner.set_status(key, text);
        }
    }

    fn set_working_message(&self, message: Option<&str>) {
        if self.runner.assert_active().is_ok() {
            self.inner.set_working_message(message);
        }
    }

    fn set_working_visible(&self, visible: bool) {
        if self.runner.assert_active().is_ok() {
            self.inner.set_working_visible(visible);
        }
    }

    fn set_working_indicator(&self, options: Option<WorkingIndicatorOptions>) {
        if self.runner.assert_active().is_ok() {
            self.inner.set_working_indicator(options);
        }
    }

    fn set_hidden_thinking_label(&self, label: Option<&str>) {
        if self.runner.assert_active().is_ok() {
            self.inner.set_hidden_thinking_label(label);
        }
    }

    fn set_title(&self, title: &str) {
        if self.runner.assert_active().is_ok() {
            self.inner.set_title(title);
        }
    }

    fn paste_to_editor(&self, text: &str) {
        if self.runner.assert_active().is_ok() {
            self.inner.paste_to_editor(text);
        }
    }

    fn set_editor_text(&self, text: &str) {
        if self.runner.assert_active().is_ok() {
            self.inner.set_editor_text(text);
        }
    }

    fn get_editor_text(&self) -> String {
        if self.runner.assert_active().is_ok() {
            self.inner.get_editor_text()
        } else {
            String::new()
        }
    }
}

/// 扩展运行时句柄：供扩展在注册期与执行期调用会话动作，并在会话替换或重载时通过 `invalidate()` 立即拦截过期访问。
#[derive(Clone)]
pub struct ExtensionRuntime {
    pub(super) inner: Arc<ExtensionRunnerInner>,
    pub(super) registration_changes: Arc<Mutex<Option<Vec<CatalogChange>>>>,
}

impl ExtensionRuntime {
    pub fn register_provider(&self, provider: Provider) -> Result<(), String> {
        self.change_catalog(CatalogChange::Provider(provider))
    }

    pub fn register_provider_config(
        &self,
        name: impl Into<String>,
        mut config: ProviderConfig,
    ) -> Result<(), String> {
        self.check_active()?;
        config.id = name.into();
        self.register_provider(create_provider(config)?)
    }

    pub fn unregister_provider(&self, name: impl Into<String>) -> Result<(), String> {
        self.change_catalog(CatalogChange::RemoveProvider(name.into()))
    }

    pub fn register_virtual_model(&self, definition: VirtualModelDefinition) -> Result<(), String> {
        self.change_catalog(CatalogChange::Virtual(definition))
    }

    pub fn unregister_virtual_model(
        &self,
        provider: impl Into<String>,
        id: impl Into<String>,
    ) -> Result<(), String> {
        self.change_catalog(CatalogChange::RemoveVirtual(provider.into(), id.into()))
    }

    fn change_catalog(&self, change: CatalogChange) -> Result<(), String> {
        self.check_active()?;
        if let Some(pending) = self
            .registration_changes
            .lock()
            .expect("registration lock")
            .as_mut()
        {
            pending.push(change);
            return Ok(());
        }
        {
            self.inner
                .catalog
                .lock()
                .expect("catalog lock")
                .apply(change);
        }
        self.inner.notify_catalog_changed();
        Ok(())
    }

    pub fn check_active(&self) -> Result<(), String> {
        self.inner.assert_active()
    }

    /// Cancelled when this runner is replaced or the session closes.
    pub fn cancellation_token(&self) -> CancellationToken {
        self.inner.invalidation.clone()
    }

    pub fn events(&self) -> &EventBus {
        self.check_active().expect("active extension runtime");
        &self.inner.event_bus
    }

    pub fn send_message(
        &self,
        message: BeforeAgentStartCustomMessageDraft,
        options: SendMessageOptions,
    ) -> Result<(), String> {
        self.inner.assert_active()?;
        let actions = self.inner.actions()?;
        (actions.send_message)(message, options)
    }

    pub fn send_user_message(
        &self,
        text: impl Into<String>,
        images: Option<Vec<ImageContent>>,
        deliver_as: Option<StreamingInputBehavior>,
    ) -> Result<(), String> {
        self.inner.assert_active()?;
        let actions = self.inner.actions()?;
        (actions.send_user_message)(text.into(), images, deliver_as)
    }

    pub fn append_entry(
        &self,
        custom_type: impl Into<String>,
        data: Option<JsonValue>,
    ) -> Result<String, String> {
        self.inner.assert_active()?;
        let actions = self.inner.actions()?;
        (actions.append_entry)(custom_type.into(), data)
    }

    pub fn set_session_name(&self, name: impl Into<String>) -> Result<String, String> {
        self.inner.assert_active()?;
        let actions = self.inner.actions()?;
        (actions.set_session_name)(name.into())
    }

    pub fn get_session_name(&self) -> Result<Option<String>, String> {
        self.inner.assert_active()?;
        let actions = self.inner.actions()?;
        Ok((actions.get_session_name)())
    }

    pub fn set_label(
        &self,
        target_id: impl Into<String>,
        label: Option<String>,
    ) -> Result<String, String> {
        self.inner.assert_active()?;
        let actions = self.inner.actions()?;
        (actions.set_label)(target_id.into(), label)
    }

    pub fn get_active_tools(&self) -> Result<Vec<String>, String> {
        self.inner.assert_active()?;
        let actions = self.inner.actions()?;
        Ok((actions.get_active_tools)())
    }

    /// Includes tools selected before their asynchronous inventory becomes available.
    pub fn get_requested_tools(&self) -> Result<Vec<String>, String> {
        self.inner.assert_active()?;
        Ok((self.inner.actions()?.get_requested_tools)())
    }

    pub fn get_all_tools(&self) -> Result<Vec<ToolInfo>, String> {
        self.inner.assert_active()?;
        let actions = self.inner.actions()?;
        Ok((actions.get_all_tools)())
    }

    pub fn set_active_tools(&self, tool_names: Vec<String>) -> Result<(), String> {
        self.inner.assert_active()?;
        let actions = self.inner.actions()?;
        (actions.set_active_tools)(tool_names)
    }

    pub fn get_mcp_servers(&self) -> Result<Vec<RegisteredMcpServer>, String> {
        self.check_active()?;
        let extensions = self.inner.extensions.read().expect("extensions lock");
        let mut servers = BTreeMap::new();
        for extension in extensions.iter() {
            servers.extend(extension.mcp_servers.clone());
        }
        Ok(servers.into_values().collect())
    }

    pub fn register_mcp_server(
        &self,
        extension_path: &str,
        name: &str,
        config: McpServerConfig,
    ) -> Result<(), String> {
        self.check_active()?;
        validate_mcp_server_config(
            name,
            &serde_json::to_value(&config).map_err(|e| e.to_string())?,
        )?;
        {
            let mut extensions = self.inner.extensions.write().expect("extensions lock");
            if extensions
                .iter()
                .flat_map(|ext| ext.mcp_servers.keys())
                .any(|other| other != name && mcp_namespace(other) == mcp_namespace(name))
            {
                return Err(format!("MCP namespace collision: {name}"));
            }
            let extension = extensions
                .iter_mut()
                .find(|ext| ext.path == extension_path)
                .ok_or_else(|| format!("Unknown extension: {extension_path}"))?;
            extension.mcp_servers.insert(
                name.to_owned(),
                RegisteredMcpServer {
                    name: name.to_owned(),
                    config,
                    source_info: extension.source_info.clone(),
                },
            );
        }
        self.inner.event_bus.emit("mcp_servers_change", Value::Null);
        Ok(())
    }

    pub fn unregister_mcp_server(&self, extension_path: &str, name: &str) -> Result<bool, String> {
        self.check_active()?;
        let removed = {
            let mut extensions = self.inner.extensions.write().expect("extensions lock");
            let extension = extensions
                .iter_mut()
                .find(|ext| ext.path == extension_path)
                .ok_or_else(|| format!("Unknown extension: {extension_path}"))?;
            extension.mcp_servers.remove(name).is_some()
        };
        if removed {
            self.inner.event_bus.emit("mcp_servers_change", Value::Null);
        }
        Ok(removed)
    }

    /// Atomically replaces one extension's tools, retaining its source and precedence.
    pub fn replace_extension_tools(
        &self,
        extension_path: &str,
        definitions: Vec<ToolDefinition>,
    ) -> Result<(), String> {
        self.check_active()?;
        {
            let mut extensions = self.inner.extensions.write().expect("extensions lock");
            let extension = extensions
                .iter_mut()
                .find(|ext| ext.path == extension_path)
                .ok_or_else(|| format!("Unknown extension: {extension_path}"))?;
            extension.tools = definitions
                .into_iter()
                .map(|definition| {
                    (
                        definition.name.clone(),
                        RegisteredTool {
                            definition,
                            source_info: extension.source_info.clone(),
                        },
                    )
                })
                .collect();
        }
        self.refresh_tools()
    }

    pub fn refresh_tools(&self) -> Result<(), String> {
        self.inner.assert_active()?;
        let actions = self.inner.actions()?;
        (actions.refresh_tools)();
        Ok(())
    }

    pub fn get_commands(&self) -> Result<Vec<SlashCommandInfo>, String> {
        self.inner.assert_active()?;
        let actions = self.inner.actions()?;
        Ok((actions.get_commands)())
    }

    pub async fn set_model(&self, model: Model) -> Result<bool, String> {
        self.inner.assert_active()?;
        let actions = self.inner.actions()?;
        (actions.set_model)(model).await
    }

    pub fn get_thinking_level(&self) -> Result<ModelThinkingLevel, String> {
        self.inner.assert_active()?;
        let actions = self.inner.actions()?;
        Ok((actions.get_thinking_level)())
    }

    pub fn set_thinking_level(&self, level: ModelThinkingLevel) -> Result<(), String> {
        self.inner.assert_active()?;
        let actions = self.inner.actions()?;
        (actions.set_thinking_level)(level);
        Ok(())
    }
}

/// 传递给事件处理器、快捷键与工具的会话上下文（内置 `assert_active` 过期保护，对照 `ExtensionContext`）。
#[derive(Clone)]
pub struct ExtensionContext {
    pub(super) inner: Arc<ExtensionRunnerInner>,
    pub(super) system_prompt_override: Option<Arc<dyn Fn() -> String + Send + Sync>>,
}

impl ExtensionContext {
    pub fn check_active(&self) -> Result<(), String> {
        self.inner.assert_active()
    }

    fn require_active(&self) {
        if let Err(msg) = self.inner.assert_active() {
            panic!("{msg}");
        }
    }

    pub fn runtime(&self) -> ExtensionRuntime {
        ExtensionRuntime {
            inner: self.inner.clone(),
            registration_changes: Arc::new(Mutex::new(None)),
        }
    }

    pub fn ui(&self) -> Arc<dyn ExtensionUiContext> {
        self.require_active();
        Arc::new(PromptTrackingUiContext {
            inner: self.inner.ui_context(),
            runner: self.inner.clone(),
        })
    }

    pub fn mode(&self) -> ExtensionMode {
        self.require_active();
        *self.inner.mode.read().expect("mode lock")
    }

    pub fn has_ui(&self) -> bool {
        self.require_active();
        self.inner.ui_context().has_ui()
    }

    pub fn cwd(&self) -> String {
        self.require_active();
        self.inner.cwd.read().expect("cwd lock").clone()
    }

    pub fn model(&self) -> Option<Model> {
        self.require_active();
        self.inner
            .context_actions
            .read()
            .expect("ctx actions lock")
            .as_ref()
            .and_then(|ca| (ca.get_model)())
    }

    /// The session-bound catalog includes provider credentials and extension overrides.
    pub fn model_catalog(&self) -> Result<Arc<Models>, String> {
        self.check_active()?;
        self.inner
            .catalog
            .lock()
            .expect("catalog lock")
            .models()
            .ok_or_else(|| "Session model catalog is not bound".to_owned())
    }

    pub fn thinking_level(&self) -> ModelThinkingLevel {
        self.require_active();
        self.inner
            .actions
            .read()
            .expect("actions lock")
            .as_ref()
            .map_or(ModelThinkingLevel::Off, |a| (a.get_thinking_level)())
    }

    pub fn is_idle(&self) -> bool {
        self.require_active();
        self.inner
            .context_actions
            .read()
            .expect("ctx actions lock")
            .as_ref()
            .is_none_or(|ca| (ca.is_idle)())
    }

    pub fn cancel_token(&self) -> Option<CancellationToken> {
        self.require_active();
        self.inner
            .context_actions
            .read()
            .expect("ctx actions lock")
            .as_ref()
            .and_then(|ca| (ca.get_cancel_token)())
    }

    pub fn abort(&self) {
        self.require_active();
        if let Some(ca) = self
            .inner
            .context_actions
            .read()
            .expect("ctx actions lock")
            .as_ref()
        {
            (ca.abort)();
        }
    }

    pub fn has_pending_messages(&self) -> bool {
        self.require_active();
        self.inner
            .context_actions
            .read()
            .expect("ctx actions lock")
            .as_ref()
            .is_some_and(|ca| (ca.has_pending_messages)())
    }

    pub fn shutdown(&self) {
        self.require_active();
        if let Some(ca) = self
            .inner
            .context_actions
            .read()
            .expect("ctx actions lock")
            .as_ref()
        {
            (ca.shutdown)();
        }
    }

    pub fn get_context_usage(&self) -> Option<ContextUsage> {
        self.require_active();
        self.inner
            .context_actions
            .read()
            .expect("ctx actions lock")
            .as_ref()
            .and_then(|ca| (ca.get_context_usage)())
    }

    pub fn compact(&self, options: Option<CompactOptions>) {
        self.require_active();
        if let Some(ca) = self
            .inner
            .context_actions
            .read()
            .expect("ctx actions lock")
            .as_ref()
        {
            (ca.compact)(options);
        }
    }

    pub fn get_system_prompt(&self) -> String {
        self.require_active();
        if let Some(override_fn) = &self.system_prompt_override {
            return override_fn();
        }
        self.inner
            .context_actions
            .read()
            .expect("ctx actions lock")
            .as_ref()
            .map_or_else(String::new, |ca| (ca.get_system_prompt)())
    }

    pub fn session_id(&self) -> String {
        self.require_active();
        self.inner
            .context_actions
            .read()
            .expect("ctx actions lock")
            .as_ref()
            .map_or_else(String::new, |ca| (ca.get_session_id)())
    }

    pub fn session_file(&self) -> Option<PathBuf> {
        self.require_active();
        self.inner
            .context_actions
            .read()
            .expect("ctx actions lock")
            .as_ref()
            .and_then(|ca| (ca.get_session_file)())
    }

    pub fn session_branch(&self) -> Vec<SessionEntry> {
        self.require_active();
        self.inner
            .context_actions
            .read()
            .expect("ctx actions lock")
            .as_ref()
            .map_or_else(Vec::new, |ca| (ca.get_session_branch)())
    }

    pub fn session_tree(&self) -> Vec<SessionTreeNode> {
        self.require_active();
        self.inner
            .context_actions
            .read()
            .expect("ctx actions lock")
            .as_ref()
            .map_or_else(Vec::new, |ca| (ca.get_session_tree)())
    }

    pub fn session_leaf_id(&self) -> Option<String> {
        self.require_active();
        self.inner
            .context_actions
            .read()
            .expect("ctx actions lock")
            .as_ref()
            .and_then(|ca| (ca.get_session_leaf_id)())
    }
}

/// 工具执行上下文（在 `ExtensionContext` 基础上附加 `tools` 与 `execute_tool` 嵌套调用入口，对照 `ExtensionToolContext`）。
#[derive(Clone)]
pub struct ExtensionToolContext {
    pub(super) base: ExtensionContext,
    pub(super) tool_call_id: String,
    pub(super) default_cancel_token: Option<CancellationToken>,
}

impl std::ops::Deref for ExtensionToolContext {
    type Target = ExtensionContext;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl ExtensionToolContext {
    pub fn tool_call_id(&self) -> &str {
        &self.tool_call_id
    }

    pub fn tools(&self) -> Vec<AgentTool> {
        self.base.require_active();
        self.base
            .inner
            .context_actions
            .read()
            .expect("ctx actions lock")
            .as_ref()
            .map_or_else(Vec::new, |ca| (ca.get_callable_tools)())
    }

    pub async fn execute_tool(
        &self,
        name: &str,
        args: JsonValue,
        mut options: NestedToolCallOptions,
    ) -> AgentToolCallOutcome {
        self.base.require_active();
        if options.cancel_token.is_none() {
            options.cancel_token = self.default_cancel_token.clone();
        }
        let exec_fn = self
            .base
            .inner
            .context_actions
            .read()
            .expect("ctx actions lock")
            .as_ref()
            .and_then(|ca| ca.execute_tool.clone());

        let Some(exec_fn) = exec_fn else {
            return AgentToolCallOutcome {
                tool_call: pi_ai::ToolCall {
                    id: format!("{}/0", self.tool_call_id),
                    name: name.to_owned(),
                    arguments: Map::new(),
                    thought_signature: None,
                    namespace: None,
                    arguments_error: None,
                },
                result: AgentToolResult::error_text(
                    "Nested tool calls are not available in this context",
                ),
                is_error: true,
                duration_ms: None,
            };
        };

        exec_fn(self.tool_call_id.clone(), name.to_owned(), args, options).await
    }
}

/// 斜杠命令执行上下文（在 `ExtensionContext` 基础上附加 `new_session`、`fork`、`navigate_tree`、`switch_session`、`wait_for_idle`）。
#[derive(Clone)]
pub struct ExtensionCommandContext {
    pub(super) base: ExtensionContext,
}

impl std::ops::Deref for ExtensionCommandContext {
    type Target = ExtensionContext;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl ExtensionCommandContext {
    pub fn get_system_prompt_options(&self) -> BuildSystemPromptOptions {
        self.base.require_active();
        self.base
            .inner
            .context_actions
            .read()
            .expect("ctx actions lock")
            .as_ref()
            .map_or_else(
                || BuildSystemPromptOptions {
                    cwd: self.base.cwd(),
                    ..BuildSystemPromptOptions::default()
                },
                |ca| (ca.get_system_prompt_options)(),
            )
    }

    pub async fn wait_for_idle(&self) {
        self.base.require_active();
        let fut = self
            .base
            .inner
            .command_actions
            .read()
            .expect("cmd actions lock")
            .as_ref()
            .map(|ca| (ca.wait_for_idle)());
        if let Some(f) = fut {
            f.await;
        }
    }

    pub async fn new_session(&self, parent_session: Option<String>) -> Result<bool, String> {
        self.base.require_active();
        let fut = self
            .base
            .inner
            .command_actions
            .read()
            .expect("cmd actions lock")
            .as_ref()
            .map(|ca| (ca.new_session)(parent_session))
            .ok_or_else(|| "Command context not bound".to_owned())?;
        fut.await
    }

    pub async fn fork(
        &self,
        entry_id: impl Into<String>,
        options: ForkSessionOptions,
    ) -> Result<bool, String> {
        self.base.require_active();
        let entry_id = entry_id.into();
        let fut = self
            .base
            .inner
            .command_actions
            .read()
            .expect("cmd actions lock")
            .as_ref()
            .map(|ca| (ca.fork)(entry_id, options))
            .ok_or_else(|| "Command context not bound".to_owned())?;
        fut.await
    }

    pub async fn navigate_tree(
        &self,
        target_id: impl Into<String>,
        options: NavigateTreeOptions,
    ) -> Result<bool, String> {
        self.base.require_active();
        let target_id = target_id.into();
        let fut = self
            .base
            .inner
            .command_actions
            .read()
            .expect("cmd actions lock")
            .as_ref()
            .map(|ca| (ca.navigate_tree)(target_id, options))
            .ok_or_else(|| "Command context not bound".to_owned())?;
        fut.await
    }

    pub async fn switch_session(&self, session_path: impl Into<PathBuf>) -> Result<bool, String> {
        self.base.require_active();
        let path = session_path.into();
        let fut = self
            .base
            .inner
            .command_actions
            .read()
            .expect("cmd actions lock")
            .as_ref()
            .map(|ca| (ca.switch_session)(path))
            .ok_or_else(|| "Command context not bound".to_owned())?;
        fut.await
    }

    pub async fn reload(&self) -> Result<(), String> {
        self.base.require_active();
        let fut = self
            .base
            .inner
            .command_actions
            .read()
            .expect("cmd actions lock")
            .as_ref()
            .and_then(|ca| ca.reload.as_ref().map(|r| r()))
            .ok_or_else(|| "Reload action is not bound in command context".to_owned())?;
        fut.await
    }
}

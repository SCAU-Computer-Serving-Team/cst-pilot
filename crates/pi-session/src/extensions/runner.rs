use super::*;

// =============================================================================
// 5. Static Extension Trait, ExtensionRegistrar & ExtensionRunner
// =============================================================================

pub type InputHandlerFn = Arc<
    dyn Fn(
            InputEvent,
            ExtensionContext,
        ) -> BoxFuture<'static, Result<Option<InputEventResult>, String>>
        + Send
        + Sync,
>;
pub type BeforeAgentStartHandlerFn = Arc<
    dyn for<'a> Fn(
            &'a mut BeforeAgentStartEvent,
            ExtensionContext,
        ) -> BoxFuture<'a, Result<Option<BeforeAgentStartEventResult>, String>>
        + Send
        + Sync,
>;
pub type ContextHandlerFn = Arc<
    dyn for<'a> Fn(
            &'a mut ContextEvent,
            ExtensionContext,
        ) -> BoxFuture<'a, Result<Option<ContextEventResult>, String>>
        + Send
        + Sync,
>;
pub type ContextWithSystemHandlerFn = Arc<
    dyn Fn(
            ContextWithSystemEvent,
            ExtensionContext,
        ) -> BoxFuture<'static, Result<Option<ContextEventResult>, String>>
        + Send
        + Sync,
>;
pub type BeforeProviderRequestHandlerFn = Arc<
    dyn Fn(
            BeforeProviderRequestEvent,
            ExtensionContext,
        ) -> BoxFuture<'static, Result<Option<JsonValue>, String>>
        + Send
        + Sync,
>;
pub type BeforeProviderHeadersHandlerFn = Arc<
    dyn for<'a> Fn(
            &'a mut BeforeProviderHeadersEvent,
            ExtensionContext,
        ) -> BoxFuture<'a, Result<(), String>>
        + Send
        + Sync,
>;
pub type AfterProviderResponseHandlerFn = Arc<
    dyn Fn(AfterProviderResponseEvent, ExtensionContext) -> BoxFuture<'static, Result<(), String>>
        + Send
        + Sync,
>;
pub type ProviderStreamEventHandlerFn = Arc<
    dyn Fn(JsonValue, ExtensionContext) -> BoxFuture<'static, Result<(), String>> + Send + Sync,
>;
pub type UserBashHandlerFn = Arc<
    dyn Fn(
            UserBashEvent,
            ExtensionContext,
        ) -> BoxFuture<'static, Result<Option<UserBashEventResult>, String>>
        + Send
        + Sync,
>;
pub type ToolCallHandlerFn = Arc<
    dyn Fn(
            ToolCallEvent,
            ExtensionContext,
        ) -> BoxFuture<'static, Result<Option<ToolCallEventResult>, String>>
        + Send
        + Sync,
>;
pub type ToolResultHandlerFn = Arc<
    dyn Fn(
            ToolResultEvent,
            ExtensionContext,
        ) -> BoxFuture<'static, Result<Option<ToolResultEventResult>, String>>
        + Send
        + Sync,
>;
pub type MessageEndHandlerFn = Arc<
    dyn Fn(
            MessageEndEvent,
            ExtensionContext,
        ) -> BoxFuture<'static, Result<Option<MessageEndEventResult>, String>>
        + Send
        + Sync,
>;
pub type BoundaryHandlerFn = Arc<
    dyn Fn(
            BoundaryEvent,
            ExtensionContext,
        ) -> BoxFuture<'static, Result<Option<BoundaryResult>, String>>
        + Send
        + Sync,
>;
pub type LifecycleHandlerFn = Arc<
    dyn Fn(
            SessionLifecycleEvent,
            ExtensionContext,
        ) -> BoxFuture<'static, Result<SessionBeforeEventOutput, String>>
        + Send
        + Sync,
>;
pub type ResourcesDiscoverHandlerFn = Arc<
    dyn Fn(
            ResourcesDiscoverEvent,
            ExtensionContext,
        ) -> BoxFuture<'static, Result<Option<ResourcesDiscoverResult>, String>>
        + Send
        + Sync,
>;
pub type CacheWarmingDecisionHandlerFn = Arc<
    dyn Fn(
            CacheWarmingDecisionEvent,
            ExtensionContext,
        ) -> BoxFuture<'static, Result<Option<CacheWarmingDecisionEventResult>, String>>
        + Send
        + Sync,
>;

/// 可重复调用的扩展构造工厂（支持在多会话、跨 `cwd` 重建与 `reload()` 中独立生成扩展实例）。
pub type ExtensionFactory =
    Arc<dyn Fn() -> Result<Box<dyn Extension>, ExtensionInitError> + Send + Sync>;

pub fn extension_factory<F>(f: F) -> ExtensionFactory
where
    F: Fn() -> Result<Box<dyn Extension>, ExtensionInitError> + Send + Sync + 'static,
{
    Arc::new(f)
}

#[derive(Clone, Default)]
struct ExtensionHandlers {
    input: Vec<InputHandlerFn>,
    before_agent_start: Vec<BeforeAgentStartHandlerFn>,
    context: Vec<ContextHandlerFn>,
    context_with_system: Vec<ContextWithSystemHandlerFn>,
    before_provider_request: Vec<BeforeProviderRequestHandlerFn>,
    before_provider_headers: Vec<BeforeProviderHeadersHandlerFn>,
    after_provider_response: Vec<AfterProviderResponseHandlerFn>,
    provider_stream_event: Vec<ProviderStreamEventHandlerFn>,
    user_bash: Vec<UserBashHandlerFn>,
    tool_call: Vec<ToolCallHandlerFn>,
    tool_result: Vec<ToolResultHandlerFn>,
    message_end: Vec<MessageEndHandlerFn>,
    turn_end: Vec<BoundaryHandlerFn>,
    agent_before_settle: Vec<BoundaryHandlerFn>,
    resources_discover: Vec<ResourcesDiscoverHandlerFn>,
    cache_warming_decision: Vec<CacheWarmingDecisionHandlerFn>,
    lifecycle: BTreeMap<String, Vec<LifecycleHandlerFn>>,
}

#[derive(Clone)]
pub struct LoadedExtension {
    pub path: String,
    pub source_info: SourceInfo,
    pub hidden: bool,
    pub replaceable: bool,
    pub tools: BTreeMap<String, RegisteredTool>,
    pub mcp_servers: BTreeMap<String, RegisteredMcpServer>,
    pub commands: BTreeMap<String, RegisteredCommand>,
    pub flags: BTreeMap<String, ExtensionFlag>,
    pub shortcuts: BTreeMap<String, ExtensionShortcut>,
    pub providers: Vec<Provider>,
    pub virtual_models: Vec<VirtualModelDefinition>,
    catalog_changes: Vec<CatalogChange>,
    pub message_renderers: BTreeMap<String, String>,
    pub entry_renderers: BTreeMap<String, String>,
    handlers: ExtensionHandlers,
}

/// 编译期静态扩展接口（替代上游动态 JS 加载器）。
pub trait Extension: Send + Sync + 'static {
    fn path(&self) -> &str;
    fn source_info(&self) -> SourceInfo {
        SourceInfo::synthetic(self.path(), "extension")
    }
    fn hidden(&self) -> bool {
        false
    }
    fn replaceable(&self) -> bool {
        false
    }
    fn register(&self, api: &mut ExtensionRegistrar) -> Result<(), ExtensionInitError>;
}

type InlineRegisterFn =
    Box<dyn Fn(&mut ExtensionRegistrar) -> Result<(), ExtensionInitError> + Send + Sync>;

/// 函数式内联扩展封装（便于测试和轻量扩展直接使用闭包注册）。
pub struct InlineExtension {
    path: String,
    source_info: SourceInfo,
    hidden: bool,
    replaceable: bool,
    register_fn: InlineRegisterFn,
}

impl InlineExtension {
    pub fn new<F>(path: impl Into<String>, register_fn: F) -> Self
    where
        F: Fn(&mut ExtensionRegistrar) -> Result<(), ExtensionInitError> + Send + Sync + 'static,
    {
        let path = path.into();
        let source_info = SourceInfo::synthetic(path.clone(), "inline");
        Self {
            path,
            source_info,
            hidden: false,
            replaceable: false,
            register_fn: Box::new(register_fn),
        }
    }

    pub fn with_source_info(mut self, source_info: SourceInfo) -> Self {
        self.source_info = source_info;
        self
    }

    pub fn with_replaceable(mut self, replaceable: bool) -> Self {
        self.replaceable = replaceable;
        self
    }

    pub fn with_hidden(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self
    }
}

impl Extension for InlineExtension {
    fn path(&self) -> &str {
        &self.path
    }

    fn source_info(&self) -> SourceInfo {
        self.source_info.clone()
    }

    fn hidden(&self) -> bool {
        self.hidden
    }

    fn replaceable(&self) -> bool {
        self.replaceable
    }

    fn register(&self, api: &mut ExtensionRegistrar) -> Result<(), ExtensionInitError> {
        (self.register_fn)(api)
    }
}

/// 扩展注册器（对照上游 `ExtensionAPI`）。
pub struct ExtensionRegistrar {
    extension_path: String,
    source_info: SourceInfo,
    runtime: ExtensionRuntime,
    tools: BTreeMap<String, RegisteredTool>,
    mcp_servers: BTreeMap<String, RegisteredMcpServer>,
    commands: BTreeMap<String, RegisteredCommand>,
    flags: BTreeMap<String, ExtensionFlag>,
    shortcuts: BTreeMap<String, ExtensionShortcut>,
    providers: Vec<Provider>,
    virtual_models: Vec<VirtualModelDefinition>,
    message_renderers: BTreeMap<String, String>,
    entry_renderers: BTreeMap<String, String>,
    handlers: ExtensionHandlers,
}

impl ExtensionRegistrar {
    pub fn extension_path(&self) -> &str {
        &self.extension_path
    }

    pub fn runtime(&self) -> ExtensionRuntime {
        self.runtime.clone()
    }

    pub fn events(&self) -> &EventBus {
        self.runtime.events()
    }

    pub fn register_mcp_server(
        &mut self,
        name: impl Into<String>,
        config: McpServerConfig,
    ) -> Result<(), String> {
        let name = name.into();
        validate_mcp_server_config(
            &name,
            &serde_json::to_value(&config).map_err(|e| e.to_string())?,
        )?;
        if self
            .mcp_servers
            .keys()
            .any(|other| other != &name && mcp_namespace(other) == mcp_namespace(&name))
        {
            return Err(format!("MCP namespace collision: {name}"));
        }
        self.mcp_servers.insert(
            name.clone(),
            RegisteredMcpServer {
                name,
                config,
                source_info: self.source_info.clone(),
            },
        );
        Ok(())
    }

    pub fn register_tool(&mut self, definition: ToolDefinition) {
        let name = definition.name.clone();
        self.tools.insert(
            name,
            RegisteredTool {
                definition,
                source_info: self.source_info.clone(),
            },
        );
    }

    pub fn register_command(&mut self, mut command: RegisteredCommand) {
        command.source_info = self.source_info.clone();
        self.commands.insert(command.name.clone(), command);
    }

    pub fn register_simple_command<F, Fut>(
        &mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        handler: F,
    ) where
        F: Fn(String, ExtensionCommandContext) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        let h = Arc::new(handler);
        self.register_command(RegisteredCommand {
            name: name.into(),
            description: Some(description.into()),
            argument_hint: None,
            source_info: self.source_info.clone(),
            handler: Arc::new(move |args, ctx| {
                let h = h.clone();
                Box::pin(async move { h(args, ctx).await })
            }),
        });
    }

    pub fn register_flag(
        &mut self,
        name: impl Into<String>,
        flag_type: ExtensionFlagType,
        default_value: Option<ExtensionFlagValue>,
        description: Option<String>,
    ) {
        let name = name.into();
        self.flags.insert(
            name.clone(),
            ExtensionFlag {
                name,
                description,
                flag_type,
                default_value,
                extension_path: self.extension_path.clone(),
            },
        );
    }

    pub fn register_shortcut<F, Fut>(
        &mut self,
        key: impl Into<String>,
        description: Option<String>,
        handler: F,
    ) where
        F: Fn(ExtensionContext) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let key = key.into();
        let h = Arc::new(handler);
        self.shortcuts.insert(
            key.clone(),
            ExtensionShortcut {
                key,
                description,
                extension_path: self.extension_path.clone(),
                handler: Arc::new(move |ctx| {
                    let h = h.clone();
                    Box::pin(async move {
                        h(ctx).await;
                    })
                }),
            },
        );
    }

    pub fn register_provider(&mut self, provider: Provider) {
        self.runtime
            .register_provider(provider.clone())
            .expect("active registrar");
        self.providers.retain(|p| p.id() != provider.id());
        self.providers.push(provider);
    }

    pub fn register_provider_config(
        &mut self,
        name: impl Into<String>,
        mut config: ProviderConfig,
    ) -> Result<(), String> {
        config.id = name.into();
        self.register_provider(create_provider(config)?);
        Ok(())
    }

    pub fn unregister_provider(&mut self, name: impl Into<String>) {
        let name = name.into();
        self.providers.retain(|p| p.id() != name);
        self.virtual_models.retain(|v| v.provider != name);
        self.runtime
            .unregister_provider(name)
            .expect("active registrar");
    }

    pub fn register_virtual_model(&mut self, definition: VirtualModelDefinition) {
        self.runtime
            .register_virtual_model(definition.clone())
            .expect("active registrar");
        self.virtual_models
            .retain(|v| v.provider != definition.provider || v.id != definition.id);
        self.virtual_models.push(definition);
    }

    pub fn unregister_virtual_model(&mut self, provider: impl Into<String>, id: impl Into<String>) {
        let provider = provider.into();
        let id = id.into();
        self.virtual_models
            .retain(|v| v.provider != provider || v.id != id);
        self.runtime
            .unregister_virtual_model(provider, id)
            .expect("active registrar");
    }

    pub fn on_user_bash(&mut self, handler: UserBashHandlerFn) {
        self.handlers.user_bash.push(handler);
    }

    pub fn register_message_renderer(
        &mut self,
        custom_type: impl Into<String>,
        renderer_id: impl Into<String>,
    ) {
        self.message_renderers
            .insert(custom_type.into(), renderer_id.into());
    }

    pub fn register_entry_renderer(
        &mut self,
        custom_type: impl Into<String>,
        renderer_id: impl Into<String>,
    ) {
        self.entry_renderers
            .insert(custom_type.into(), renderer_id.into());
    }

    pub fn on_input<F, Fut>(&mut self, handler: F)
    where
        F: Fn(InputEvent, ExtensionContext) -> Fut + Send + Sync + 'static,
        Fut:
            std::future::Future<Output = Result<Option<InputEventResult>, String>> + Send + 'static,
    {
        let h = Arc::new(handler);
        self.handlers.input.push(Arc::new(move |ev, ctx| {
            let h = h.clone();
            Box::pin(async move { h(ev, ctx).await })
        }));
    }

    pub fn on_before_agent_start(&mut self, handler: BeforeAgentStartHandlerFn) {
        self.handlers.before_agent_start.push(handler);
    }

    pub fn on_context(&mut self, handler: ContextHandlerFn) {
        self.handlers.context.push(handler);
    }

    pub fn on_context_with_system<F, Fut>(&mut self, handler: F)
    where
        F: Fn(ContextWithSystemEvent, ExtensionContext) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Option<ContextEventResult>, String>>
            + Send
            + 'static,
    {
        let h = Arc::new(handler);
        self.handlers
            .context_with_system
            .push(Arc::new(move |ev, ctx| {
                let h = h.clone();
                Box::pin(async move { h(ev, ctx).await })
            }));
    }

    pub fn on_before_provider_request<F, Fut>(&mut self, handler: F)
    where
        F: Fn(BeforeProviderRequestEvent, ExtensionContext) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Option<JsonValue>, String>> + Send + 'static,
    {
        let h = Arc::new(handler);
        self.handlers
            .before_provider_request
            .push(Arc::new(move |ev, ctx| {
                let h = h.clone();
                Box::pin(async move { h(ev, ctx).await })
            }));
    }

    pub fn on_before_provider_headers(&mut self, handler: BeforeProviderHeadersHandlerFn) {
        self.handlers.before_provider_headers.push(handler);
    }

    pub fn on_after_provider_response<F, Fut>(&mut self, handler: F)
    where
        F: Fn(AfterProviderResponseEvent, ExtensionContext) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        let h = Arc::new(handler);
        self.handlers
            .after_provider_response
            .push(Arc::new(move |ev, ctx| {
                let h = h.clone();
                Box::pin(async move { h(ev, ctx).await })
            }));
    }

    pub fn on_provider_stream_event<F, Fut>(&mut self, handler: F)
    where
        F: Fn(JsonValue, ExtensionContext) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        let h = Arc::new(handler);
        self.handlers
            .provider_stream_event
            .push(Arc::new(move |ev, ctx| {
                let h = h.clone();
                Box::pin(async move { h(ev, ctx).await })
            }));
    }

    pub fn on_tool_call<F, Fut>(&mut self, handler: F)
    where
        F: Fn(ToolCallEvent, ExtensionContext) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Option<ToolCallEventResult>, String>>
            + Send
            + 'static,
    {
        let h = Arc::new(handler);
        self.handlers.tool_call.push(Arc::new(move |ev, ctx| {
            let h = h.clone();
            Box::pin(async move { h(ev, ctx).await })
        }));
    }

    pub fn on_tool_result<F, Fut>(&mut self, handler: F)
    where
        F: Fn(ToolResultEvent, ExtensionContext) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Option<ToolResultEventResult>, String>>
            + Send
            + 'static,
    {
        let h = Arc::new(handler);
        self.handlers.tool_result.push(Arc::new(move |ev, ctx| {
            let h = h.clone();
            Box::pin(async move { h(ev, ctx).await })
        }));
    }

    pub fn on_message_end<F, Fut>(&mut self, handler: F)
    where
        F: Fn(MessageEndEvent, ExtensionContext) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Option<MessageEndEventResult>, String>>
            + Send
            + 'static,
    {
        let h = Arc::new(handler);
        self.handlers.message_end.push(Arc::new(move |ev, ctx| {
            let h = h.clone();
            Box::pin(async move { h(ev, ctx).await })
        }));
    }

    pub fn on_turn_end<F, Fut>(&mut self, handler: F)
    where
        F: Fn(BoundaryEvent, ExtensionContext) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Option<BoundaryResult>, String>> + Send + 'static,
    {
        let h = Arc::new(handler);
        self.handlers.turn_end.push(Arc::new(move |ev, ctx| {
            let h = h.clone();
            Box::pin(async move { h(ev, ctx).await })
        }));
    }

    pub fn on_agent_before_settle<F, Fut>(&mut self, handler: F)
    where
        F: Fn(BoundaryEvent, ExtensionContext) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Option<BoundaryResult>, String>> + Send + 'static,
    {
        let h = Arc::new(handler);
        self.handlers
            .agent_before_settle
            .push(Arc::new(move |ev, ctx| {
                let h = h.clone();
                Box::pin(async move { h(ev, ctx).await })
            }));
    }

    pub fn on_lifecycle<F, Fut>(&mut self, event_name: impl Into<String>, handler: F)
    where
        F: Fn(SessionLifecycleEvent, ExtensionContext) -> Fut + Send + Sync + 'static,
        Fut:
            std::future::Future<Output = Result<SessionBeforeEventOutput, String>> + Send + 'static,
    {
        let h = Arc::new(handler);
        self.handlers
            .lifecycle
            .entry(event_name.into())
            .or_default()
            .push(Arc::new(move |ev, ctx| {
                let h = h.clone();
                Box::pin(async move { h(ev, ctx).await })
            }));
    }

    pub fn on_resources_discover<F, Fut>(&mut self, handler: F)
    where
        F: Fn(ResourcesDiscoverEvent, ExtensionContext) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Option<ResourcesDiscoverResult>, String>>
            + Send
            + 'static,
    {
        let h = Arc::new(handler);
        self.handlers
            .resources_discover
            .push(Arc::new(move |ev, ctx| {
                let h = h.clone();
                Box::pin(async move { h(ev, ctx).await })
            }));
    }

    pub fn on_cache_warming_decision<F, Fut>(&mut self, handler: F)
    where
        F: Fn(CacheWarmingDecisionEvent, ExtensionContext) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Option<CacheWarmingDecisionEventResult>, String>>
            + Send
            + 'static,
    {
        let h = Arc::new(handler);
        self.handlers
            .cache_warming_decision
            .push(Arc::new(move |ev, ctx| {
                let h = h.clone();
                Box::pin(async move { h(ev, ctx).await })
            }));
    }
}

/// 解决 `replaceable: true` 扩展与普通扩展的命名冲突：
/// 若某个 `replaceable` 扩展注册的工具、命令或标志被其他非被替换扩展注册，则该 `replaceable` 扩展整包退让。
fn resolve_replaceable_extensions(
    mut loaded: Vec<LoadedExtension>,
    warnings: &mut Vec<ResourceDiagnostic>,
) -> Vec<LoadedExtension> {
    let mut dropped_indices: BTreeSet<usize> = BTreeSet::new();

    for (idx, ext) in loaded.iter().enumerate() {
        if !ext.replaceable {
            continue;
        }
        let conflict = loaded.iter().enumerate().find_map(|(other_idx, other)| {
            if other_idx == idx || dropped_indices.contains(&other_idx) {
                return None;
            }
            if let Some(tool_name) = ext.tools.keys().find(|k| other.tools.contains_key(*k)) {
                return Some((other.path.clone(), "tool", tool_name.clone()));
            }
            if let Some(cmd_name) = ext
                .commands
                .keys()
                .find(|k| other.commands.contains_key(*k))
            {
                return Some((other.path.clone(), "command", format!("/{cmd_name}")));
            }
            if let Some(flag_name) = ext.flags.keys().find(|k| other.flags.contains_key(*k)) {
                return Some((other.path.clone(), "flag", format!("--{flag_name}")));
            }
            None
        });
        if let Some((other_path, kind, registered_name)) = conflict {
            if let Some(builtin_name) = ext.path.strip_prefix("builtin:") {
                warnings.push(ResourceDiagnostic {
                    kind: "warning".to_owned(),
                    message: format!(
                        "Extension {other_path} registers {kind} `{registered_name}`, so built-in extension `{builtin_name}` was not loaded."
                    ),
                    path: ext.path.clone(),
                });
            }
            dropped_indices.insert(idx);
        }
    }

    let mut idx = 0usize;
    loaded.retain(|_| {
        let keep = !dropped_indices.contains(&idx);
        idx += 1;
        keep
    });
    loaded
}

pub type CatalogChangedListener = Arc<dyn Fn() + Send + Sync>;

pub(super) struct ExtensionRunnerInner {
    pub(super) invalidation: CancellationToken,
    pub(super) catalog: Arc<Mutex<ExtensionCatalog>>,
    pub(super) catalog_changed_listener: Mutex<Option<CatalogChangedListener>>,
    pub(super) extensions: RwLock<Vec<LoadedExtension>>,
    pub(super) event_bus: EventBus,
    pub(super) ui_context: RwLock<Arc<dyn ExtensionUiContext>>,
    pub(super) mode: RwLock<ExtensionMode>,
    pub(super) cwd: RwLock<String>,
    pub(super) stale_message: RwLock<Option<String>>,
    pub(super) actions: RwLock<Option<ExtensionActions>>,
    pub(super) context_actions: RwLock<Option<ExtensionContextActions>>,
    pub(super) command_actions: RwLock<Option<ExtensionCommandContextActions>>,
    pub(super) flag_values: RwLock<BTreeMap<String, ExtensionFlagValue>>,
    pub(super) error_listeners: Mutex<Vec<ExtensionErrorListener>>,
    pub(super) shortcut_diagnostics: Mutex<Vec<ResourceDiagnostic>>,
    pub(super) omitted_warnings: Mutex<Vec<ResourceDiagnostic>>,
    pub(super) ui_prompt_depth: AtomicUsize,
    pub(super) active_ui_prompt: Mutex<Option<(UiPromptKind, Option<String>)>>,
}

impl ExtensionRunnerInner {
    pub(super) fn notify_catalog_changed(&self) {
        let cb = self
            .catalog_changed_listener
            .lock()
            .expect("catalog listener lock")
            .clone();
        if let Some(cb) = cb {
            cb();
        }
    }
    pub(super) fn assert_active(&self) -> Result<(), String> {
        if let Some(msg) = self.stale_message.read().expect("stale lock").as_ref() {
            return Err(msg.clone());
        }
        Ok(())
    }

    pub(super) fn actions(&self) -> Result<ExtensionActions, String> {
        self.actions
            .read()
            .expect("actions lock")
            .clone()
            .ok_or_else(|| "ExtensionRuntime actions are not bound yet".to_owned())
    }

    pub(super) fn ui_context(&self) -> Arc<dyn ExtensionUiContext> {
        self.ui_context.read().expect("ui lock").clone()
    }

    pub(super) fn emit_error(&self, error: ExtensionError) {
        let listeners: Vec<ExtensionErrorListener> = self
            .error_listeners
            .lock()
            .expect("error listeners lock")
            .clone();
        for listener in listeners {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(&error)));
        }
    }

    pub(super) async fn with_ui_prompt<T>(
        self: &Arc<Self>,
        kind: UiPromptKind,
        title: Option<String>,
        fut: BoxFuture<'_, T>,
    ) -> T {
        let prev = self.ui_prompt_depth.fetch_add(1, Ordering::SeqCst);
        if prev == 0 {
            *self.active_ui_prompt.lock().expect("ui prompt lock") = Some((kind, title.clone()));
            let _ = self
                .emit_lifecycle(SessionLifecycleEvent::UiPromptStart {
                    kind,
                    title: title.clone(),
                })
                .await;
        }

        let res = fut.await;

        let after = self.ui_prompt_depth.fetch_sub(1, Ordering::SeqCst);
        if after == 1 {
            let active = self
                .active_ui_prompt
                .lock()
                .expect("ui prompt lock")
                .take()
                .unwrap_or((kind, title));
            let _ = self
                .emit_lifecycle(SessionLifecycleEvent::UiPromptEnd {
                    kind: active.0,
                    title: active.1,
                })
                .await;
        }
        res
    }

    async fn emit_lifecycle(
        self: &Arc<Self>,
        event: SessionLifecycleEvent,
    ) -> SessionBeforeEventOutput {
        let event_name = event.event_name();
        let is_before = matches!(
            event,
            SessionLifecycleEvent::SessionBeforeSwitch { .. }
                | SessionLifecycleEvent::SessionBeforeFork { .. }
                | SessionLifecycleEvent::SessionBeforeCompact { .. }
                | SessionLifecycleEvent::SessionBeforeTree { .. }
        );

        let snapshot: Vec<(String, Vec<LifecycleHandlerFn>)> = {
            let exts = self.extensions.read().expect("extensions lock");
            exts.iter()
                .filter_map(|ext| {
                    ext.handlers
                        .lifecycle
                        .get(event_name)
                        .filter(|h| !h.is_empty())
                        .map(|h| (ext.path.clone(), h.clone()))
                })
                .collect()
        };

        let ctx = ExtensionContext {
            inner: self.clone(),
            system_prompt_override: None,
        };
        let mut last_output = SessionBeforeEventOutput::None;

        for (ext_path, handlers) in snapshot {
            for handler in handlers {
                match isolate_handler(async { handler(event.clone(), ctx.clone()).await }).await {
                    Ok(out) => {
                        if is_before {
                            let cancelled = match &out {
                                SessionBeforeEventOutput::Switch(r) => r.cancel,
                                SessionBeforeEventOutput::Fork(r) => r.cancel,
                                SessionBeforeEventOutput::Compact(r) => r.cancel,
                                SessionBeforeEventOutput::Tree(r) => r.cancel,
                                SessionBeforeEventOutput::None => false,
                            };
                            if !matches!(out, SessionBeforeEventOutput::None) {
                                last_output = out;
                            }
                            if cancelled {
                                return last_output;
                            }
                        }
                    }
                    Err(err) => {
                        self.emit_error(ExtensionError {
                            extension_path: ext_path.clone(),
                            event: event_name.to_owned(),
                            error: err,
                        });
                    }
                }
            }
        }

        last_output
    }
}

#[derive(Debug, Clone)]
pub struct BoundaryDispatchResult {
    pub entries: Vec<SessionBoundaryDraft>,
    pub r#continue: bool,
    pub context: BoundaryContextPreview,
    pub valid: bool,
}

#[derive(Debug, Clone)]
pub struct BeforeAgentStartCombinedResult {
    pub messages: Vec<BeforeAgentStartCustomMessageDraft>,
    pub system_prompt_options: NormalizedBuildSystemPromptOptions,
}

/// 静态扩展运行器（对照上游 `ExtensionRunner`）。
#[derive(Clone)]
pub struct ExtensionRunner {
    inner: Arc<ExtensionRunnerInner>,
}

impl std::fmt::Debug for ExtensionRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionRunner")
            .field("extension_count", &self.get_extension_paths().len())
            .finish()
    }
}

impl ExtensionRunner {
    pub fn from_extensions(
        cwd: impl Into<String>,
        extensions: Vec<Box<dyn Extension>>,
    ) -> Result<Self, ExtensionInitError> {
        let inner = Arc::new(ExtensionRunnerInner {
            invalidation: CancellationToken::new(),
            catalog: Arc::new(Mutex::new(ExtensionCatalog::default())),
            catalog_changed_listener: Mutex::new(None),
            extensions: RwLock::new(Vec::new()),
            event_bus: EventBus::new(),
            ui_context: RwLock::new(Arc::new(NoOpUiContext::new())),
            mode: RwLock::new(ExtensionMode::Print),
            cwd: RwLock::new(cwd.into()),
            stale_message: RwLock::new(None),
            actions: RwLock::new(None),
            context_actions: RwLock::new(None),
            command_actions: RwLock::new(None),
            flag_values: RwLock::new(BTreeMap::new()),
            error_listeners: Mutex::new(Vec::new()),
            shortcut_diagnostics: Mutex::new(Vec::new()),
            omitted_warnings: Mutex::new(Vec::new()),
            ui_prompt_depth: AtomicUsize::new(0),
            active_ui_prompt: Mutex::new(None),
        });

        let mut loaded = Vec::with_capacity(extensions.len());

        for ext in extensions {
            let runtime = ExtensionRuntime {
                inner: inner.clone(),
                registration_changes: Arc::new(Mutex::new(Some(Vec::new()))),
            };
            let mut registrar = ExtensionRegistrar {
                extension_path: ext.path().to_owned(),
                source_info: ext.source_info(),
                runtime: runtime.clone(),
                tools: BTreeMap::new(),
                mcp_servers: BTreeMap::new(),
                commands: BTreeMap::new(),
                flags: BTreeMap::new(),
                shortcuts: BTreeMap::new(),
                providers: Vec::new(),
                virtual_models: Vec::new(),
                message_renderers: BTreeMap::new(),
                entry_renderers: BTreeMap::new(),
                handlers: ExtensionHandlers::default(),
            };
            ext.register(&mut registrar)?;

            for (name, flag) in &registrar.flags {
                if let Some(def) = &flag.default_value {
                    inner
                        .flag_values
                        .write()
                        .expect("flag values lock")
                        .entry(name.clone())
                        .or_insert_with(|| def.clone());
                }
            }

            loaded.push(LoadedExtension {
                path: registrar.extension_path,
                source_info: registrar.source_info,
                hidden: ext.hidden(),
                replaceable: ext.replaceable(),
                tools: registrar.tools,
                mcp_servers: registrar.mcp_servers,
                commands: registrar.commands,
                flags: registrar.flags,
                shortcuts: registrar.shortcuts,
                providers: registrar.providers,
                virtual_models: registrar.virtual_models,
                catalog_changes: runtime
                    .registration_changes
                    .lock()
                    .expect("registration lock")
                    .take()
                    .unwrap_or_default(),
                message_renderers: registrar.message_renderers,
                entry_renderers: registrar.entry_renderers,
                handlers: registrar.handlers,
            });
        }

        let mut omitted_warnings = Vec::new();
        let resolved = resolve_replaceable_extensions(loaded, &mut omitted_warnings);
        *inner.omitted_warnings.lock().expect("warnings lock") = omitted_warnings;
        for ext in &resolved {
            for change in &ext.catalog_changes {
                inner
                    .catalog
                    .lock()
                    .expect("catalog lock")
                    .apply(change.clone());
            }
        }
        *inner.extensions.write().expect("extensions lock") = resolved;

        Ok(Self { inner })
    }

    pub fn from_factories(
        cwd: impl Into<String>,
        factories: &[ExtensionFactory],
    ) -> Result<Self, ExtensionInitError> {
        let mut exts = Vec::with_capacity(factories.len());
        for factory in factories {
            exts.push(factory()?);
        }
        Self::from_extensions(cwd, exts)
    }

    pub fn empty(cwd: impl Into<String>) -> Self {
        Self::from_extensions(cwd, Vec::new()).expect("empty extensions")
    }

    pub fn runtime(&self) -> ExtensionRuntime {
        ExtensionRuntime {
            inner: self.inner.clone(),
            registration_changes: Arc::new(Mutex::new(None)),
        }
    }

    /// 注册目录变更监听器（在目录锁释放后触发）。
    pub fn set_on_catalog_changed(&self, listener: Option<CatalogChangedListener>) {
        *self
            .inner
            .catalog_changed_listener
            .lock()
            .expect("catalog listener lock") = listener;
    }

    /// Bind the application catalog and publish queued extension registrations.
    pub fn bind_models(&self, models: Arc<Models>) {
        {
            self.inner
                .catalog
                .lock()
                .expect("catalog lock")
                .bind(models);
        }
        self.inner.notify_catalog_changed();
    }

    pub fn detach_models(&self) {
        {
            self.inner.catalog.lock().expect("catalog lock").detach();
        }
        self.inner.notify_catalog_changed();
    }

    pub fn bind_core(&self, actions: ExtensionActions, context_actions: ExtensionContextActions) {
        *self.inner.actions.write().expect("actions lock") = Some(actions);
        *self
            .inner
            .context_actions
            .write()
            .expect("ctx actions lock") = Some(context_actions);
    }

    pub fn bind_command_context(&self, actions: Option<ExtensionCommandContextActions>) {
        *self
            .inner
            .command_actions
            .write()
            .expect("cmd actions lock") = actions;
    }

    pub fn set_ui_context(
        &self,
        ui_context: Option<Arc<dyn ExtensionUiContext>>,
        mode: ExtensionMode,
    ) {
        let ui: Arc<dyn ExtensionUiContext> =
            ui_context.unwrap_or_else(|| Arc::new(NoOpUiContext::new()));
        *self.inner.ui_context.write().expect("ui lock") = ui;
        *self.inner.mode.write().expect("mode lock") = mode;
    }

    pub(crate) fn inherit_ui_from(&self, previous: &Self) {
        *self
            .inner
            .error_listeners
            .lock()
            .expect("error listeners lock") = previous
            .inner
            .error_listeners
            .lock()
            .expect("error listeners lock")
            .clone();
        *self.inner.flag_values.write().expect("flags lock") = previous
            .inner
            .flag_values
            .read()
            .expect("flags lock")
            .clone();
        self.set_ui_context(
            Some(previous.inner.ui_context()),
            *previous.inner.mode.read().expect("mode lock"),
        );
    }

    pub fn ui_context(&self) -> Arc<dyn ExtensionUiContext> {
        Arc::new(PromptTrackingUiContext {
            inner: self.inner.ui_context(),
            runner: self.inner.clone(),
        })
    }

    pub fn has_ui(&self) -> bool {
        self.inner.ui_context().has_ui()
    }

    pub fn invalidate(&self, message: Option<String>) {
        let mut guard = self.inner.stale_message.write().expect("stale lock");
        if guard.is_none() {
            *guard = Some(message.unwrap_or_else(|| DEFAULT_STALE_CONTEXT_MESSAGE.to_owned()));
        }
        drop(guard);
        self.inner.invalidation.cancel();
        self.inner.actions.write().expect("actions lock").take();
        self.inner
            .context_actions
            .write()
            .expect("context actions lock")
            .take();
        self.inner
            .command_actions
            .write()
            .expect("command actions lock")
            .take();
    }

    pub fn is_invalidated(&self) -> bool {
        self.inner
            .stale_message
            .read()
            .expect("stale lock")
            .is_some()
    }

    /// 构造共享同一套已加载扩展与设置、但拥有全新活跃世代句柄的 `ExtensionRunner`
    /// （供会话替换 `new_session`/`fork`/`switch_session` 在作废旧上下文后使用）。
    pub fn clone_with_fresh_generation(&self) -> Self {
        let new_inner = Arc::new(ExtensionRunnerInner {
            invalidation: CancellationToken::new(),
            catalog: self.inner.catalog.clone(),
            catalog_changed_listener: Mutex::new(
                self.inner
                    .catalog_changed_listener
                    .lock()
                    .expect("catalog listener lock")
                    .clone(),
            ),
            extensions: RwLock::new(
                self.inner
                    .extensions
                    .read()
                    .expect("extensions lock")
                    .clone(),
            ),
            event_bus: self.inner.event_bus.clone(),
            ui_context: RwLock::new(self.inner.ui_context()),
            mode: RwLock::new(*self.inner.mode.read().expect("mode lock")),
            cwd: RwLock::new(self.inner.cwd.read().expect("cwd lock").clone()),
            stale_message: RwLock::new(None),
            actions: RwLock::new(None),
            context_actions: RwLock::new(None),
            command_actions: RwLock::new(None),
            flag_values: RwLock::new(
                self.inner
                    .flag_values
                    .read()
                    .expect("flag values lock")
                    .clone(),
            ),
            error_listeners: Mutex::new(
                self.inner
                    .error_listeners
                    .lock()
                    .expect("error listeners lock")
                    .clone(),
            ),
            shortcut_diagnostics: Mutex::new(
                self.inner
                    .shortcut_diagnostics
                    .lock()
                    .expect("shortcut diag lock")
                    .clone(),
            ),
            omitted_warnings: Mutex::new(
                self.inner
                    .omitted_warnings
                    .lock()
                    .expect("warnings lock")
                    .clone(),
            ),
            ui_prompt_depth: AtomicUsize::new(0),
            active_ui_prompt: Mutex::new(None),
        });
        Self { inner: new_inner }
    }

    pub fn on_error(&self, listener: ExtensionErrorListener) {
        self.inner
            .error_listeners
            .lock()
            .expect("error listeners lock")
            .push(listener);
    }

    pub fn emit_error(&self, error: ExtensionError) {
        self.inner.emit_error(error);
    }

    pub fn get_extension_paths(&self) -> Vec<String> {
        self.inner
            .extensions
            .read()
            .expect("extensions lock")
            .iter()
            .map(|e| e.path.clone())
            .collect()
    }

    pub fn get_registered_providers(&self) -> Vec<Provider> {
        self.inner
            .catalog
            .lock()
            .expect("catalog lock")
            .providers
            .values()
            .cloned()
            .collect()
    }

    pub fn get_registered_virtual_models(&self) -> Vec<VirtualModelDefinition> {
        self.inner
            .catalog
            .lock()
            .expect("catalog lock")
            .virtual_models
            .values()
            .cloned()
            .collect()
    }

    pub fn get_virtual_model(&self, provider: &str, id: &str) -> Option<VirtualModelDefinition> {
        self.inner
            .catalog
            .lock()
            .expect("catalog lock")
            .virtual_models
            .get(&(provider.to_owned(), id.to_owned()))
            .cloned()
    }

    pub fn get_omitted_extension_warnings(&self) -> Vec<ResourceDiagnostic> {
        self.inner
            .omitted_warnings
            .lock()
            .expect("warnings lock")
            .clone()
    }

    /// 获取全部已注册工具（同名工具按扩展加载顺序「先注册者胜出」）。
    pub fn get_all_registered_tools(&self) -> Vec<RegisteredTool> {
        let exts = self.inner.extensions.read().expect("extensions lock");
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for ext in exts.iter() {
            for tool in ext.tools.values() {
                if seen.insert(tool.definition.name.clone()) {
                    out.push(tool.clone());
                }
            }
        }
        out
    }

    pub fn get_tool_definition(&self, tool_name: &str) -> Option<ToolDefinition> {
        let exts = self.inner.extensions.read().expect("extensions lock");
        for ext in exts.iter() {
            if let Some(t) = ext.tools.get(tool_name) {
                return Some(t.definition.clone());
            }
        }
        None
    }

    pub fn get_registered_commands(&self) -> Vec<ResolvedCommand> {
        let exts = self.inner.extensions.read().expect("extensions lock");
        let mut commands: Vec<RegisteredCommand> = Vec::new();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();

        for ext in exts.iter() {
            for cmd in ext.commands.values() {
                commands.push(cmd.clone());
                *counts.entry(cmd.name.clone()).or_insert(0) += 1;
            }
        }

        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        let mut taken_names: BTreeSet<String> = BTreeSet::new();
        let mut resolved = Vec::with_capacity(commands.len());

        for cmd in commands {
            let occ = {
                let entry = seen.entry(cmd.name.clone()).or_insert(0);
                *entry += 1;
                *entry
            };
            let total = counts.get(&cmd.name).copied().unwrap_or(1);
            let mut invocation_name = if total > 1 {
                format!("{}:{occ}", cmd.name)
            } else {
                cmd.name.clone()
            };
            if taken_names.contains(&invocation_name) {
                let mut suffix = occ;
                loop {
                    suffix += 1;
                    let candidate = format!("{}:{suffix}", cmd.name);
                    if !taken_names.contains(&candidate) {
                        invocation_name = candidate;
                        break;
                    }
                }
            }
            taken_names.insert(invocation_name.clone());
            resolved.push(ResolvedCommand {
                command: cmd,
                invocation_name,
            });
        }
        resolved
    }

    pub fn get_command(&self, invocation_name: &str) -> Option<ResolvedCommand> {
        self.get_registered_commands()
            .into_iter()
            .find(|c| c.invocation_name == invocation_name)
    }

    pub fn get_flags(&self) -> BTreeMap<String, ExtensionFlag> {
        let exts = self.inner.extensions.read().expect("extensions lock");
        let mut out = BTreeMap::new();
        for ext in exts.iter() {
            for (name, flag) in &ext.flags {
                out.entry(name.clone()).or_insert_with(|| flag.clone());
            }
        }
        out
    }

    pub fn set_flag_value(&self, name: impl Into<String>, value: ExtensionFlagValue) {
        self.inner
            .flag_values
            .write()
            .expect("flag values lock")
            .insert(name.into(), value);
    }

    pub fn get_flag_values(&self) -> BTreeMap<String, ExtensionFlagValue> {
        self.inner
            .flag_values
            .read()
            .expect("flag values lock")
            .clone()
    }

    pub fn get_shortcuts(
        &self,
        builtin_keybindings: &BTreeMap<String, Vec<String>>,
    ) -> BTreeMap<String, ExtensionShortcut> {
        let mut diagnostics = Vec::new();
        let mut builtin_by_key: BTreeMap<String, (String, bool)> = BTreeMap::new();

        for (action_id, keys) in builtin_keybindings {
            let restrict =
                RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS.contains(&action_id.as_str());
            for key in keys {
                let norm = key.to_lowercase();
                if let Some((_, existing_restrict)) = builtin_by_key.get(&norm)
                    && *existing_restrict
                    && !restrict
                {
                    continue;
                }
                builtin_by_key.insert(norm, (action_id.clone(), restrict));
            }
        }

        let exts = self.inner.extensions.read().expect("extensions lock");
        let mut resolved: BTreeMap<String, ExtensionShortcut> = BTreeMap::new();

        for ext in exts.iter() {
            for (key, shortcut) in &ext.shortcuts {
                let norm = key.to_lowercase();
                if let Some((action_id, restrict)) = builtin_by_key.get(&norm) {
                    if *restrict {
                        diagnostics.push(ResourceDiagnostic {
                            kind: "warning".to_owned(),
                            message: format!(
                                "Extension shortcut '{key}' from {} conflicts with built-in shortcut '{action_id}'. Skipping.",
                                shortcut.extension_path
                            ),
                            path: shortcut.extension_path.clone(),
                        });
                        continue;
                    }
                    diagnostics.push(ResourceDiagnostic {
                        kind: "warning".to_owned(),
                        message: format!(
                            "Extension shortcut conflict: '{key}' is built-in shortcut for {action_id} and {}. Using {}.",
                            shortcut.extension_path, shortcut.extension_path
                        ),
                        path: shortcut.extension_path.clone(),
                    });
                }

                if let Some(prev) = resolved.get(&norm) {
                    diagnostics.push(ResourceDiagnostic {
                        kind: "warning".to_owned(),
                        message: format!(
                            "Extension shortcut conflict: '{key}' registered by both {} and {}. Using {}.",
                            prev.extension_path, shortcut.extension_path, shortcut.extension_path
                        ),
                        path: shortcut.extension_path.clone(),
                    });
                }
                resolved.insert(norm, shortcut.clone());
            }
        }

        *self
            .inner
            .shortcut_diagnostics
            .lock()
            .expect("shortcut diag lock") = diagnostics;
        resolved
    }

    pub fn get_shortcut_diagnostics(&self) -> Vec<ResourceDiagnostic> {
        self.inner
            .shortcut_diagnostics
            .lock()
            .expect("shortcut diag lock")
            .clone()
    }

    pub fn create_context(&self) -> ExtensionContext {
        ExtensionContext {
            inner: self.inner.clone(),
            system_prompt_override: None,
        }
    }

    pub fn create_tool_context(
        &self,
        tool_call_id: impl Into<String>,
        cancel_token: Option<CancellationToken>,
    ) -> ExtensionToolContext {
        ExtensionToolContext {
            base: self.create_context(),
            tool_call_id: tool_call_id.into(),
            default_cancel_token: cancel_token,
        }
    }

    pub fn create_command_context(&self) -> ExtensionCommandContext {
        ExtensionCommandContext {
            base: self.create_context(),
        }
    }

    // =========================================================================
    // Event Dispatch Methods
    // =========================================================================

    pub async fn emit_lifecycle(&self, event: SessionLifecycleEvent) -> SessionBeforeEventOutput {
        self.inner.emit_lifecycle(event).await
    }

    pub async fn emit_input(
        &self,
        text: String,
        images: Option<Vec<ImageContent>>,
        source: InputSource,
        streaming_behavior: Option<StreamingInputBehavior>,
    ) -> InputEventResult {
        let snapshot: Vec<(String, Vec<InputHandlerFn>)> = {
            let exts = self.inner.extensions.read().expect("extensions lock");
            exts.iter()
                .filter(|e| !e.handlers.input.is_empty())
                .map(|e| (e.path.clone(), e.handlers.input.clone()))
                .collect()
        };

        let ctx = self.create_context();
        let orig_text = text.clone();
        let orig_images = images.clone();
        let mut current_text = text;
        let mut current_images = images;

        for (ext_path, handlers) in snapshot {
            for handler in handlers {
                let ev = InputEvent {
                    text: current_text.clone(),
                    images: current_images.clone(),
                    source,
                    streaming_behavior,
                };
                match isolate_handler(async { handler(ev, ctx.clone()).await }).await {
                    Ok(Some(InputEventResult::Handled)) => return InputEventResult::Handled,
                    Ok(Some(InputEventResult::Transform { text, images })) => {
                        current_text = text;
                        if images.is_some() {
                            current_images = images;
                        }
                    }
                    Ok(Some(InputEventResult::Continue) | None) => {}
                    Err(err) => {
                        self.emit_error(ExtensionError {
                            extension_path: ext_path.clone(),
                            event: "input".to_owned(),
                            error: err,
                        });
                    }
                }
            }
        }

        if current_text != orig_text || current_images != orig_images {
            InputEventResult::Transform {
                text: current_text,
                images: current_images,
            }
        } else {
            InputEventResult::Continue
        }
    }

    pub async fn emit_before_agent_start(
        &self,
        trigger: BeforeAgentStartTrigger,
        system_prompt_options: &BuildSystemPromptOptions,
    ) -> BeforeAgentStartCombinedResult {
        let snapshot: Vec<(String, Vec<BeforeAgentStartHandlerFn>)> = {
            let exts = self.inner.extensions.read().expect("extensions lock");
            exts.iter()
                .filter(|e| !e.handlers.before_agent_start.is_empty())
                .map(|e| (e.path.clone(), e.handlers.before_agent_start.clone()))
                .collect()
        };

        let shared_opts = BeforeAgentStartPromptOptions {
            inner: Arc::new(Mutex::new(normalize_build_system_prompt_options(
                system_prompt_options,
            ))),
        };
        let opts_for_ctx = shared_opts.clone();
        let ctx = ExtensionContext {
            inner: self.inner.clone(),
            system_prompt_override: Some(Arc::new(move || opts_for_ctx.render())),
        };

        let mut injected_messages = Vec::new();

        for (ext_path, handlers) in snapshot {
            for handler in handlers {
                let mut ev = BeforeAgentStartEvent {
                    trigger: trigger.clone(),
                    system_prompt_options: shared_opts.clone(),
                };
                let res = isolate_handler(async { handler(&mut ev, ctx.clone()).await }).await;

                match res {
                    Ok(Some(out)) => {
                        if let Some(msg) = out.message {
                            injected_messages.push(msg);
                        }
                        if let Some(forced) = out.system_prompt {
                            shared_opts
                                .update(|options| options.force_system_prompt = Some(forced));
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        self.emit_error(ExtensionError {
                            extension_path: ext_path.clone(),
                            event: "before_agent_start".to_owned(),
                            error: err,
                        });
                    }
                }
            }
        }

        let final_opts = shared_opts.snapshot();
        BeforeAgentStartCombinedResult {
            messages: injected_messages,
            system_prompt_options: final_opts,
        }
    }

    /// 两阶段请求前上下文转换：
    /// 1. `context` 处理器仅看到非 `system` 消息；若改动了消息列表，自动调用 `restore_system_messages` 将重放合并后的基准 `SystemMessage` 挂回下标 0。
    /// 2. `context_with_system` 处理器看到完整消息列表；若删掉了下标 0 的 `SystemMessage`，发出 `ExtensionError` 告警。
    pub async fn emit_context(&self, messages: Vec<AgentMessage>) -> Vec<AgentMessage> {
        let (ctx_snapshot, sys_snapshot) = {
            let exts = self.inner.extensions.read().expect("extensions lock");
            let c: Vec<(String, Vec<ContextHandlerFn>)> = exts
                .iter()
                .filter(|e| !e.handlers.context.is_empty())
                .map(|e| (e.path.clone(), e.handlers.context.clone()))
                .collect();
            let s: Vec<(String, Vec<ContextWithSystemHandlerFn>)> = exts
                .iter()
                .filter(|e| !e.handlers.context_with_system.is_empty())
                .map(|e| (e.path.clone(), e.handlers.context_with_system.clone()))
                .collect();
            (c, s)
        };

        let ctx = self.create_context();
        let mut current_messages = messages;

        for (ext_path, handlers) in ctx_snapshot {
            for handler in handlers {
                let visible_messages: Vec<AgentMessage> = current_messages
                    .iter()
                    .filter(|m| m.role() != "system")
                    .cloned()
                    .collect();
                let visible_snapshot = visible_messages.clone();
                let mut ev = ContextEvent {
                    messages: visible_messages,
                };
                match isolate_handler(async { handler(&mut ev, ctx.clone()).await }).await {
                    Ok(res) => {
                        let returned = res.and_then(|r| r.messages).or_else(|| {
                            if ev.messages != visible_snapshot {
                                Some(ev.messages)
                            } else {
                                None
                            }
                        });
                        if let Some(ret) = returned {
                            current_messages =
                                restore_system_messages(&current_messages, &visible_snapshot, ret);
                        }
                    }
                    Err(err) => {
                        self.emit_error(ExtensionError {
                            extension_path: ext_path.clone(),
                            event: "context".to_owned(),
                            error: err,
                        });
                    }
                }
            }
        }

        for (ext_path, handlers) in sys_snapshot {
            for handler in handlers {
                let had_leading_system = current_messages
                    .first()
                    .is_some_and(|m| m.role() == "system");
                let ev = ContextWithSystemEvent {
                    messages: current_messages.clone(),
                };
                match isolate_handler(async { handler(ev, ctx.clone()).await }).await {
                    Ok(res) => {
                        if let Some(msgs) = res.and_then(|r| r.messages) {
                            current_messages = msgs;
                        }
                        if had_leading_system
                            && current_messages
                                .first()
                                .is_none_or(|m| m.role() != "system")
                        {
                            self.emit_error(ExtensionError {
                                extension_path: ext_path.clone(),
                                event: "context_with_system".to_owned(),
                                error: "Handler removed the leading system message; the request has no prompt or initial tool declarations. Keep it at index 0 or replace a dropped prefix with getCurrentSystemMessage().".to_owned(),
                            });
                        }
                    }
                    Err(err) => {
                        self.emit_error(ExtensionError {
                            extension_path: ext_path.clone(),
                            event: "context_with_system".to_owned(),
                            error: err,
                        });
                    }
                }
            }
        }

        current_messages
    }

    pub async fn emit_before_provider_request(&self, payload: JsonValue) -> JsonValue {
        let snapshot: Vec<(String, Vec<BeforeProviderRequestHandlerFn>)> = {
            let exts = self.inner.extensions.read().expect("extensions lock");
            exts.iter()
                .filter(|e| !e.handlers.before_provider_request.is_empty())
                .map(|e| (e.path.clone(), e.handlers.before_provider_request.clone()))
                .collect()
        };
        let ctx = self.create_context();
        let mut current = payload;

        for (ext_path, handlers) in snapshot {
            for handler in handlers {
                let ev = BeforeProviderRequestEvent {
                    payload: current.clone(),
                };
                match isolate_handler(async { handler(ev, ctx.clone()).await }).await {
                    Ok(Some(replaced)) => current = replaced,
                    Ok(None) => {}
                    Err(err) => {
                        self.emit_error(ExtensionError {
                            extension_path: ext_path.clone(),
                            event: "before_provider_request".to_owned(),
                            error: err,
                        });
                    }
                }
            }
        }
        current
    }

    pub async fn emit_before_provider_headers(
        &self,
        mut headers: ProviderHeaders,
    ) -> ProviderHeaders {
        let snapshot: Vec<(String, Vec<BeforeProviderHeadersHandlerFn>)> = {
            let exts = self.inner.extensions.read().expect("extensions lock");
            exts.iter()
                .filter(|e| !e.handlers.before_provider_headers.is_empty())
                .map(|e| (e.path.clone(), e.handlers.before_provider_headers.clone()))
                .collect()
        };
        let ctx = self.create_context();

        for (ext_path, handlers) in snapshot {
            for handler in handlers {
                let mut ev = BeforeProviderHeadersEvent {
                    headers: headers.clone(),
                };
                match isolate_handler(async { handler(&mut ev, ctx.clone()).await }).await {
                    Ok(()) => headers = pi_ai::api::normalize_header_changes(&headers, ev.headers),
                    Err(err) => {
                        self.emit_error(ExtensionError {
                            extension_path: ext_path.clone(),
                            event: "before_provider_headers".to_owned(),
                            error: err,
                        });
                    }
                }
            }
        }
        headers
    }

    pub async fn emit_after_provider_response(&self, event: AfterProviderResponseEvent) {
        let snapshot: Vec<(String, Vec<AfterProviderResponseHandlerFn>)> = {
            let exts = self.inner.extensions.read().expect("extensions lock");
            exts.iter()
                .filter(|e| !e.handlers.after_provider_response.is_empty())
                .map(|e| (e.path.clone(), e.handlers.after_provider_response.clone()))
                .collect()
        };
        let ctx = self.create_context();
        for (ext_path, handlers) in snapshot {
            for handler in handlers {
                if let Err(err) =
                    isolate_handler(async { handler(event.clone(), ctx.clone()).await }).await
                {
                    self.emit_error(ExtensionError {
                        extension_path: ext_path.clone(),
                        event: "after_provider_response".to_owned(),
                        error: err,
                    });
                }
            }
        }
    }

    pub async fn emit_provider_stream_event(&self, event: JsonValue) {
        let snapshot: Vec<(String, Vec<ProviderStreamEventHandlerFn>)> = {
            let exts = self.inner.extensions.read().expect("extensions lock");
            exts.iter()
                .filter(|e| !e.handlers.provider_stream_event.is_empty())
                .map(|e| (e.path.clone(), e.handlers.provider_stream_event.clone()))
                .collect()
        };
        let ctx = self.create_context();
        for (ext_path, handlers) in snapshot {
            for handler in handlers {
                if let Err(err) =
                    isolate_handler(async { handler(event.clone(), ctx.clone()).await }).await
                {
                    self.emit_error(ExtensionError {
                        extension_path: ext_path.clone(),
                        event: "provider_stream_event".to_owned(),
                        error: err,
                    });
                }
            }
        }
    }

    /// Permission/execution hook errors stop the command and propagate to its caller.
    pub async fn emit_user_bash(
        &self,
        event: UserBashEvent,
    ) -> Result<Option<UserBashEventResult>, String> {
        let handlers: Vec<_> = self
            .inner
            .extensions
            .read()
            .expect("extensions lock")
            .iter()
            .flat_map(|ext| ext.handlers.user_bash.clone())
            .collect();
        let ctx = self.create_context();
        let mut result = None;
        for handler in handlers {
            if let Some(next) =
                isolate_handler(async { handler(event.clone(), ctx.clone()).await }).await?
            {
                if matches!(next, UserBashEventResult::Result(_)) {
                    return Ok(Some(next));
                }
                result = Some(next);
            }
        }
        Ok(result)
    }

    pub async fn emit_tool_call(
        &self,
        event: ToolCallEvent,
    ) -> Result<Option<ToolCallEventResult>, String> {
        let snapshot: Vec<(String, Vec<ToolCallHandlerFn>)> = {
            let exts = self.inner.extensions.read().expect("extensions lock");
            exts.iter()
                .filter(|e| !e.handlers.tool_call.is_empty())
                .map(|e| (e.path.clone(), e.handlers.tool_call.clone()))
                .collect()
        };
        let ctx = self.create_context();
        let mut last_result = None;

        for (_ext_path, handlers) in snapshot {
            for handler in handlers {
                let res =
                    isolate_handler(async { handler(event.clone(), ctx.clone()).await }).await?;
                if let Some(r) = res {
                    let blocked = r.block;
                    last_result = Some(r);
                    if blocked {
                        return Ok(last_result);
                    }
                }
            }
        }
        Ok(last_result)
    }

    pub async fn emit_tool_result(&self, event: ToolResultEvent) -> Option<ToolResultEventResult> {
        let snapshot: Vec<(String, Vec<ToolResultHandlerFn>)> = {
            let exts = self.inner.extensions.read().expect("extensions lock");
            exts.iter()
                .filter(|e| !e.handlers.tool_result.is_empty())
                .map(|e| (e.path.clone(), e.handlers.tool_result.clone()))
                .collect()
        };
        let ctx = self.create_context();
        let mut current = event;
        let mut modified = false;

        for (ext_path, handlers) in snapshot {
            for handler in handlers {
                match isolate_handler(async { handler(current.clone(), ctx.clone()).await }).await {
                    Ok(Some(res)) => {
                        if let Some(content) = res.content {
                            current.content = content;
                            if res.structured_content.is_none() {
                                current.structured_content = None;
                            }
                            modified = true;
                        }
                        if let Some(details) = res.details {
                            current.details = Some(details);
                            modified = true;
                        }
                        if let Some(sc) = res.structured_content {
                            current.structured_content = Some(sc);
                            modified = true;
                        }
                        if let Some(is_err) = res.is_error {
                            current.is_error = is_err;
                            modified = true;
                        }
                        if let Some(u) = res.usage {
                            current.usage = Some(u);
                            modified = true;
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        self.emit_error(ExtensionError {
                            extension_path: ext_path.clone(),
                            event: "tool_result".to_owned(),
                            error: err,
                        });
                    }
                }
            }
        }

        modified.then_some(ToolResultEventResult {
            content: Some(current.content),
            details: current.details,
            structured_content: current.structured_content,
            is_error: Some(current.is_error),
            usage: current.usage,
        })
    }

    pub async fn emit_message_end(&self, message: AgentMessage) -> Option<AgentMessage> {
        let snapshot: Vec<(String, Vec<MessageEndHandlerFn>)> = {
            let exts = self.inner.extensions.read().expect("extensions lock");
            exts.iter()
                .filter(|e| !e.handlers.message_end.is_empty())
                .map(|e| (e.path.clone(), e.handlers.message_end.clone()))
                .collect()
        };
        let ctx = self.create_context();
        let mut current = message;
        let mut modified = false;

        for (ext_path, handlers) in snapshot {
            for handler in handlers {
                let ev = MessageEndEvent {
                    message: current.clone(),
                };
                match isolate_handler(async { handler(ev, ctx.clone()).await }).await {
                    Ok(Some(res)) => {
                        if let Some(next_msg) = res.message {
                            if next_msg.role() != current.role() {
                                self.emit_error(ExtensionError {
                                    extension_path: ext_path.clone(),
                                    event: "message_end".to_owned(),
                                    error: "message_end handlers must return a message with the same role".to_owned(),
                                });
                                continue;
                            }
                            current = next_msg;
                            modified = true;
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        self.emit_error(ExtensionError {
                            extension_path: ext_path.clone(),
                            event: "message_end".to_owned(),
                            error: err,
                        });
                    }
                }
            }
        }

        modified.then_some(current)
    }

    pub async fn emit_boundary<F>(
        &self,
        kind: BoundaryEventKind,
        build_context: F,
    ) -> BoundaryDispatchResult
    where
        F: Fn(&[SessionBoundaryDraft]) -> Result<BoundaryContextPreview, String>,
    {
        let event_name = match &kind {
            BoundaryEventKind::TurnEnd { .. } => "turn_end",
            BoundaryEventKind::AgentBeforeSettle { .. } => "agent_before_settle",
        };
        let snapshot: Vec<(String, Vec<BoundaryHandlerFn>)> = {
            let exts = self.inner.extensions.read().expect("extensions lock");
            exts.iter()
                .filter_map(|e| {
                    let list = match &kind {
                        BoundaryEventKind::TurnEnd { .. } => &e.handlers.turn_end,
                        BoundaryEventKind::AgentBeforeSettle { .. } => {
                            &e.handlers.agent_before_settle
                        }
                    };
                    (!list.is_empty()).then(|| (e.path.clone(), list.clone()))
                })
                .collect()
        };

        let ctx = self.create_context();
        let mut entries: Vec<SessionBoundaryDraft> = Vec::new();
        let mut should_continue = false;
        let mut context = build_context(&entries).unwrap_or(BoundaryContextPreview {
            messages: Vec::new(),
            estimated_tokens: 0,
            can_continue: false,
        });
        let mut valid = true;

        for (ext_path, handlers) in snapshot {
            for handler in handlers {
                let ev = BoundaryEvent {
                    kind: kind.clone(),
                    entries: entries.clone(),
                    r#continue: should_continue,
                    context: context.clone(),
                };
                match isolate_handler(async { handler(ev, ctx.clone()).await }).await {
                    Ok(Some(res)) => {
                        if let Some(next_entries) = res.entries {
                            entries = next_entries;
                        }
                        if let Some(next_cont) = res.r#continue {
                            should_continue = next_cont;
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        self.emit_error(ExtensionError {
                            extension_path: ext_path.clone(),
                            event: event_name.to_owned(),
                            error: err,
                        });
                    }
                }

                match build_context(&entries) {
                    Ok(next_ctx) => {
                        context = next_ctx;
                        valid = true;
                    }
                    Err(err) => {
                        valid = false;
                        self.emit_error(ExtensionError {
                            extension_path: ext_path.clone(),
                            event: event_name.to_owned(),
                            error: format!("Invalid boundary entries: {err}"),
                        });
                    }
                }
            }
        }

        if valid {
            BoundaryDispatchResult {
                entries,
                r#continue: should_continue,
                context,
                valid: true,
            }
        } else {
            BoundaryDispatchResult {
                entries: Vec::new(),
                r#continue: false,
                context,
                valid: false,
            }
        }
    }

    pub async fn emit_resources_discover(
        &self,
        cwd: &str,
        reason: &str,
    ) -> Vec<(SourceInfo, ResourcesDiscoverResult)> {
        let snapshot: Vec<(String, SourceInfo, Vec<ResourcesDiscoverHandlerFn>)> = {
            let exts = self.inner.extensions.read().expect("extensions lock");
            exts.iter()
                .filter(|e| !e.handlers.resources_discover.is_empty())
                .map(|e| {
                    (
                        e.path.clone(),
                        e.source_info.clone(),
                        e.handlers.resources_discover.clone(),
                    )
                })
                .collect()
        };
        let ctx = self.create_context();
        let ev = ResourcesDiscoverEvent {
            cwd: cwd.to_owned(),
            reason: reason.to_owned(),
        };
        let mut results = Vec::new();
        for (ext_path, source_info, handlers) in snapshot {
            for handler in handlers {
                match isolate_handler(async { handler(ev.clone(), ctx.clone()).await }).await {
                    Ok(Some(res)) => results.push((source_info.clone(), res)),
                    Ok(None) => {}
                    Err(err) => {
                        self.emit_error(ExtensionError {
                            extension_path: ext_path.clone(),
                            event: "resources_discover".to_owned(),
                            error: err,
                        });
                    }
                }
            }
        }
        results
    }

    pub async fn emit_cache_warming_decision(
        &self,
        mut event: CacheWarmingDecisionEvent,
    ) -> CacheWarmingAction {
        let snapshot: Vec<(String, Vec<CacheWarmingDecisionHandlerFn>)> = {
            let exts = self.inner.extensions.read().expect("extensions lock");
            exts.iter()
                .filter(|e| !e.handlers.cache_warming_decision.is_empty())
                .map(|e| (e.path.clone(), e.handlers.cache_warming_decision.clone()))
                .collect()
        };
        let ctx = self.create_context();
        for (ext_path, handlers) in snapshot {
            for handler in handlers {
                match isolate_handler(async { handler(event.clone(), ctx.clone()).await }).await {
                    Ok(Some(res)) => {
                        if let Some(action) = res.action {
                            event.action = action;
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        self.emit_error(ExtensionError {
                            extension_path: ext_path.clone(),
                            event: "cache_warming_decision".to_owned(),
                            error: err,
                        });
                    }
                }
            }
        }
        event.action
    }
}

pub fn restore_system_messages(
    current: &[AgentMessage],
    visible_snapshot: &[AgentMessage],
    returned: Vec<AgentMessage>,
) -> Vec<AgentMessage> {
    if returned == visible_snapshot {
        return current.to_vec();
    }
    if let Some(head) = get_agent_current_system_message(current) {
        let mut out = Vec::with_capacity(returned.len() + 1);
        out.push(AgentMessage::System(head));
        out.extend(returned);
        out
    } else {
        returned
    }
}

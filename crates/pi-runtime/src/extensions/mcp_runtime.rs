use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_session::{
    ExtensionCommandContext, ExtensionContext, ExtensionRegistrar, ExtensionRuntime,
    RegisteredCommand, RegisteredMcpServer, SessionBeforeEventOutput, SourceInfo,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::mcp::{
    McpExtensionOptions, PreconnectedMcpServer, create_default_transport, mcp_tool_definitions,
};
use crate::config_value::{ConfigValueResolver, ConfigValueSource};
use crate::mcp::{
    LoadedMcpConfig, McpClient, McpClientOptions, McpExposure, McpOAuthSession, McpOAuthStore,
    McpRequestOptions, McpServerConfig, McpServerEntry, McpTransport, StreamableHttpTransport,
    StreamableHttpTransportOptions, add_mcp_server_config, load_mcp_config_with_registered,
    remove_mcp_server_config, set_mcp_server_overrides, update_mcp_server_config,
    validate_mcp_server_config,
};

type SharedRuntime = Arc<Mutex<Arc<McpRuntime>>>;

fn command_word(input: &str) -> (&str, &str) {
    let input = input.trim();
    input
        .split_once(char::is_whitespace)
        .map_or((input, ""), |(word, rest)| (word, rest.trim()))
}

pub(super) struct McpRuntime {
    options: McpExtensionOptions,
    config: LoadedMcpConfig,
    servers: Mutex<Vec<PreconnectedMcpServer>>,
    clients: Mutex<Vec<Arc<McpClient>>>,
    oauth: Mutex<BTreeMap<String, Arc<McpOAuthSession>>>,
    pending: Mutex<BTreeSet<String>>,
    errors: Mutex<BTreeMap<String, String>>,
    generation: Mutex<Option<CancellationToken>>,
    started: AtomicBool,
    waited: AtomicBool,
    cancel: CancellationToken,
    changed: watch::Sender<u64>,
    publish: tokio::sync::Mutex<()>,
}

impl McpRuntime {
    fn new(options: McpExtensionOptions, registered: &[RegisteredMcpServer]) -> Arc<Self> {
        let config = match (&options.agent_dir, &options.cwd) {
            (Some(agent_dir), Some(cwd)) => {
                load_mcp_config_with_registered(agent_dir, cwd, options.project_trusted, registered)
            }
            _ => LoadedMcpConfig::default(),
        };
        let pending = config
            .servers
            .iter()
            .filter(|entry| {
                entry.config.enabled()
                    && !options
                        .preconnected_servers
                        .iter()
                        .any(|server| server.name == entry.name)
            })
            .map(|entry| entry.name.clone())
            .collect();
        let (changed, _) = watch::channel(0);
        Arc::new(Self {
            servers: Mutex::new(options.preconnected_servers.clone()),
            options,
            config,
            pending: Mutex::new(pending),
            clients: Mutex::new(Vec::new()),
            oauth: Mutex::new(BTreeMap::new()),
            errors: Mutex::new(BTreeMap::new()),
            started: AtomicBool::new(false),
            waited: AtomicBool::new(false),
            generation: Mutex::new(None),
            cancel: CancellationToken::new(),
            changed,
            publish: tokio::sync::Mutex::new(()),
        })
    }

    pub(super) fn install(registrar: &mut ExtensionRegistrar, options: McpExtensionOptions) {
        let state = Arc::new(Mutex::new(Self::new(options, &[])));
        for definition in mcp_tool_definitions(
            &state
                .lock()
                .expect("MCP generation lock")
                .servers
                .lock()
                .expect("servers lock"),
        )
        .0
        {
            registrar.register_tool(definition);
        }
        let on_start = state.clone();
        registrar.on_lifecycle("session_start", move |_event, context| {
            let shared = on_start.clone();
            async move {
                let runtime = context.runtime();
                let registered = runtime.get_mcp_servers()?;
                let state = {
                    let mut current = shared.lock().expect("MCP generation lock");
                    let stale = current
                        .generation
                        .lock()
                        .expect("generation lock")
                        .as_ref()
                        .is_some_and(CancellationToken::is_cancelled);
                    if current.started.load(Ordering::SeqCst) && !stale {
                        return Ok(SessionBeforeEventOutput::None);
                    }
                    *current = Self::new(current.options.clone(), &registered);
                    current.clone()
                };
                state.start(&context)?;
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                let unsubscribe = runtime.events().on(
                    "mcp_servers_change",
                    Arc::new(move |_| {
                        let _ = tx.send(());
                    }),
                );
                let invalidation = runtime.cancellation_token();
                tokio::spawn(async move {
                    let mut owned = state;
                    loop {
                        tokio::select! {
                            biased;
                            () = invalidation.cancelled() => break,
                            change = rx.recv() => { if change.is_none() { break; } }
                        }
                        while rx.try_recv().is_ok() {}
                        let old = owned.clone();
                        old.close().await;
                        if runtime.check_active().is_err() {
                            break;
                        }
                        let next = match runtime.get_mcp_servers() {
                            Ok(registered) => Self::new(old.options.clone(), &registered),
                            Err(_) => break,
                        };
                        {
                            let mut current = shared.lock().expect("MCP generation lock");
                            if invalidation.is_cancelled() {
                                break;
                            }
                            *current = next.clone();
                        }
                        owned = next.clone();
                        if let Err(error) = next.start(&context) {
                            context
                                .ui()
                                .notify(&error, pi_session::UiNotifyLevel::Error);
                        }
                    }
                    unsubscribe();
                    owned.close().await;
                });
                Ok(SessionBeforeEventOutput::None)
            }
        });
        let before_prompt = state.clone();
        registrar.on_before_agent_start(Arc::new(move |_event, context| {
            let state = before_prompt.lock().expect("MCP generation lock").clone();
            Box::pin(async move {
                if !state.waited.swap(true, Ordering::SeqCst) {
                    state.wait(true, context.cancel_token()).await;
                }
                Ok(None)
            })
        }));
        let before_tool = state.clone();
        registrar.on_tool_call(move |event, context| {
            let state = before_tool.lock().expect("MCP generation lock").clone();
            async move {
                if matches!(
                    event.tool_name.as_str(),
                    "codemode"
                        | "tool_search"
                        | "list_mcp_resources"
                        | "list_mcp_resource_templates"
                        | "read_mcp_resource"
                ) {
                    state.wait(false, context.cancel_token()).await;
                }
                Ok(None)
            }
        });
        let on_shutdown = state.clone();
        registrar.on_lifecycle("session_shutdown", move |_event, _context| {
            let state = on_shutdown.lock().expect("MCP generation lock").clone();
            async move {
                state.close().await;
                Ok(SessionBeforeEventOutput::None)
            }
        });
        registrar.register_command(RegisteredCommand {
            name: "mcp".to_owned(),
            description: Some("Manage MCP servers, exposure and authentication".to_owned()),
            argument_hint: Some("[status|auth NAME|logout NAME|reconnect [NAME]|add SCOPE NAME JSON|update SCOPE NAME JSON|remove SCOPE NAME]".to_owned()),
            source_info: SourceInfo::builtin("builtin:mcp"),
            handler: Arc::new(move |args, context| {
                let shared = state.clone();
                Box::pin(async move { Self::command(shared, args, context).await })
            }),
        });
    }

    fn start(self: &Arc<Self>, context: &ExtensionContext) -> Result<(), String> {
        if self.started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let runtime = context.runtime();
        *self.generation.lock().expect("generation lock") = Some(runtime.cancellation_token());
        runtime.replace_extension_tools(
            "builtin:mcp",
            mcp_tool_definitions(&self.servers.lock().expect("servers lock")).0,
        )?;
        self.activate_helpers(&runtime)?;
        for entry in &self.config.servers {
            if !self
                .pending
                .lock()
                .expect("pending lock")
                .contains(&entry.name)
            {
                continue;
            }
            let entry = entry.clone();
            let state = self.clone();
            let runtime = runtime.clone();
            let catalog = context.model_catalog().ok();
            tokio::spawn(async move {
                let result = tokio::select! {
                    biased;
                    () = state.cancel.cancelled() => Err("MCP connection cancelled".to_owned()),
                    result = state.connect(entry.clone(), runtime, catalog) => result,
                };
                if let Err(error) = result {
                    state
                        .errors
                        .lock()
                        .expect("errors lock")
                        .insert(entry.name.clone(), error);
                }
                state
                    .pending
                    .lock()
                    .expect("pending lock")
                    .remove(&entry.name);
                state.changed.send_modify(|value| *value += 1);
            });
        }
        Ok(())
    }

    async fn command(
        shared: SharedRuntime,
        args: String,
        context: ExtensionCommandContext,
    ) -> Result<(), String> {
        let state = shared.lock().expect("MCP generation lock").clone();
        let (action, rest) = command_word(&args);
        match action {
            "" | "status" => {
                if !rest.is_empty() { return Err("Usage: /mcp status".to_owned()); }
                let mut lines = state.config.errors.clone();
                lines.extend(state.servers.lock().expect("servers lock").iter().map(|server| format!("{} (connected)", server.name)));
                lines.extend(state.pending.lock().expect("pending lock").iter().map(|name| format!("{name} (connecting)")));
                lines.extend(state.errors.lock().expect("errors lock").iter().map(|(name, error)| format!("{name} (failed): {error}")));
                lines.extend(state.config.servers.iter().filter(|entry| !entry.config.enabled()).map(|entry| format!("{} (disabled)", entry.name)));
                if lines.is_empty() { lines.push("No MCP servers configured.".to_owned()); }
                context.ui().notify(&lines.join("\n"), pi_session::UiNotifyLevel::Info);
                return Ok(());
            }
            "auth" | "logout" => {
                if rest.is_empty() || rest.contains(char::is_whitespace) { return Err(format!("Usage: /mcp {action} NAME")); }
                let entry = state.config.servers.iter().find(|entry| entry.name == rest).ok_or_else(|| format!("Unknown MCP server: {rest}"))?;
                let oauth = state.oauth_session(entry).await?;
                if action == "logout" {
                    oauth.logout().await?;
                } else {
                    let ui = context.ui();
                    let redirect = state.options.on_oauth_redirect.clone().unwrap_or_else(|| Arc::new(move |url| {
                        let ui = ui.clone();
                        Box::pin(async move {
                            ui.notify(&format!("Open this URL to authorize MCP:\n{url}"), pi_session::UiNotifyLevel::Info);
                            Ok(())
                        })
                    }));
                    tokio::select! {
                        biased;
                        () = state.cancel.cancelled() => return Err("MCP authorization cancelled".to_owned()),
                        result = oauth.login(redirect, context.cancel_token().unwrap_or_else(|| context.runtime().cancellation_token())) => result?,
                    };
                }
            }
            "reconnect" => {
                if !rest.is_empty() && !state.config.servers.iter().any(|entry| entry.name == rest) {
                    return Err(format!("Unknown MCP server: {rest}"));
                }
            }
            "add" | "update" | "remove" => {
                let (scope, rest) = command_word(rest);
                let (name, body) = command_word(rest);
                if name.is_empty() { return Err(format!("Usage: /mcp {action} global|project NAME [JSON]")); }
                let path = match scope {
                    "global" => state.options.agent_dir.as_ref().ok_or("Missing MCP agent directory")?.join("mcp.json"),
                    "project" if state.options.project_trusted => state.options.cwd.as_ref().ok_or("Missing MCP working directory")?.join(crate::settings_manager::CONFIG_DIR_NAME).join("mcp.json"),
                    "project" => return Err("Project MCP configuration requires trust".to_owned()),
                    _ => return Err("MCP configuration scope must be global or project".to_owned()),
                };
                let base = state.config.servers.iter().find(|entry| entry.name == name).map(|entry| entry.config.clone());
                let is_project = scope == "project";
                let (action, name, body) = (action.to_owned(), name.to_owned(), body.to_owned());
                tokio::task::spawn_blocking(move || {
                    if action == "remove" {
                        if !body.is_empty() { return Err("Usage: /mcp remove global|project NAME".to_owned()); }
                        return remove_mcp_server_config(&path, &name);
                    }
                    let value: serde_json::Value = serde_json::from_str(&body).map_err(|error| format!("Invalid MCP JSON: {error}"))?;
                    if is_project && value.get("auth").is_some_and(|auth| !auth.is_null()) {
                        return Err("Provider auth is only allowed in the global mcp.json".to_owned());
                    }
                    if action == "add" {
                        let config = validate_mcp_server_config(&name, &value)?;
                        return add_mcp_server_config(&path, &name, &config);
                    }
                    let exposure_only = value.as_object().is_some_and(|fields| !fields.is_empty() && fields.keys().all(|key| ["enabled", "exposure", "toolExposure"].contains(&key.as_str())));
                    if exposure_only && let Some(base) = base {
                        set_mcp_server_overrides(&path, &name, &value, &base)
                    } else {
                        update_mcp_server_config(&path, &name, &value)
                    }
                }).await.map_err(|_| "MCP configuration write task failed")??;
            }
            _ => return Err("Usage: /mcp [status|auth NAME|logout NAME|reconnect [NAME]|add SCOPE NAME JSON|update SCOPE NAME JSON|remove SCOPE NAME]. Use update with enabled, exposure or toolExposure fields.".to_owned()),
        }
        context
            .runtime()
            .events()
            .emit("mcp_servers_change", serde_json::Value::Null);
        Ok(())
    }

    async fn oauth_session(&self, entry: &McpServerEntry) -> Result<Arc<McpOAuthSession>, String> {
        if let Some(session) = self
            .oauth
            .lock()
            .expect("OAuth sessions lock")
            .get(&entry.name)
            .cloned()
        {
            return Ok(session);
        }
        let McpServerConfig::Http(http) = &entry.config else {
            return Err("MCP OAuth requires an HTTP server".to_owned());
        };
        if http.auth.is_some() {
            return Err("This MCP server uses provider authentication".to_owned());
        }
        let mut config = http.oauth.clone().unwrap_or_default();
        let agent_dir = self
            .options
            .agent_dir
            .as_ref()
            .ok_or("Missing MCP agent directory")?;
        if let Some(secret) = &config.client_secret {
            let cwd = self
                .options
                .cwd
                .as_ref()
                .ok_or("Missing MCP working directory")?;
            config.client_secret = Some(
                ConfigValueResolver::new(cwd, agent_dir, self.options.project_trusted)
                    .resolve_async(secret.clone(), None, Self::value_source(entry))
                    .await?
                    .ok_or("Could not resolve MCP OAuth client secret")?,
            );
        }
        let oauth = Arc::new(McpOAuthSession::new(
            &entry.name,
            &http.url,
            config,
            McpOAuthStore::new(agent_dir),
        )?);
        let mut sessions = self.oauth.lock().expect("OAuth sessions lock");
        Ok(sessions.entry(entry.name.clone()).or_insert(oauth).clone())
    }

    fn value_source(entry: &McpServerEntry) -> ConfigValueSource {
        if entry.scope.as_deref() == Some("project") {
            ConfigValueSource::Project
        } else {
            ConfigValueSource::Global
        }
    }

    fn activate_helpers(&self, runtime: &ExtensionRuntime) -> Result<(), String> {
        let servers = self.servers.lock().expect("servers lock");
        let (_, mut codemode, mut search) = mcp_tool_definitions(&servers);
        for entry in &self.config.servers {
            if !entry.config.enabled() {
                continue;
            }
            codemode |= entry.config.exposure() == McpExposure::Codemode
                || entry
                    .config
                    .tool_exposure()
                    .values()
                    .any(|e| *e == McpExposure::Codemode);
            search |= entry.config.exposure() == McpExposure::Deferred
                || entry
                    .config
                    .tool_exposure()
                    .values()
                    .any(|e| *e == McpExposure::Deferred);
        }
        let mut active = runtime.get_requested_tools()?;
        let existing = active.clone();
        if codemode
            && self
                .options
                .auto_enable_codemode
                .or(self.config.auto_enable_codemode)
                .unwrap_or(true)
            && !active.iter().any(|name| name == "codemode")
        {
            active.push("codemode".to_owned());
        }
        if search && !active.iter().any(|name| name == "tool_search") {
            active.push("tool_search".to_owned());
        }
        if active != existing {
            runtime.set_active_tools(active)?;
        }
        Ok(())
    }

    async fn connect(
        self: &Arc<Self>,
        mut entry: McpServerEntry,
        runtime: ExtensionRuntime,
        models: Option<Arc<pi_ai::Models>>,
    ) -> Result<(), String> {
        let cwd = self
            .options
            .cwd
            .as_ref()
            .ok_or("Missing MCP working directory")?;
        let agent_dir = self
            .options
            .agent_dir
            .as_ref()
            .ok_or("Missing MCP agent directory")?;
        let resolver = ConfigValueResolver::new(cwd, agent_dir, self.options.project_trusted);
        let source = Self::value_source(&entry);
        let values = match &mut entry.config {
            McpServerConfig::Stdio(config) => &mut config.env,
            McpServerConfig::Http(config) => &mut config.headers,
        };
        for (key, value) in values {
            *value = resolver
                .resolve_async(value.clone(), None, source)
                .await?
                .ok_or_else(|| format!("Could not resolve MCP configuration field {key}"))?;
        }
        if let McpServerConfig::Http(http) = &mut entry.config
            && let Some(auth) = &http.auth
        {
            let models =
                models.ok_or("MCP provider authentication requires a session model catalog")?;
            let resolved = models
                .get_auth(&auth.provider, None)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("No authentication configured for {}", auth.provider))?;
            if let Some(key) = resolved.auth.api_key {
                http.headers
                    .insert("Authorization".to_owned(), format!("Bearer {key}"));
            }
            for (name, value) in resolved.auth.headers.unwrap_or_default() {
                if let Some(value) = value {
                    http.headers.insert(name, value);
                }
            }
        }
        let transport: Arc<dyn McpTransport> = if let McpServerConfig::Http(http) = &entry.config {
            let oauth = if http.auth.is_none() {
                Some(self.oauth_session(&entry).await?)
            } else {
                None
            };
            Arc::new(StreamableHttpTransport::with_options(
                StreamableHttpTransportOptions {
                    url: http.url.clone(),
                    headers: http.headers.clone(),
                    oauth,
                    ..Default::default()
                },
            ))
        } else {
            create_default_transport(&entry, cwd, agent_dir)
        };
        let client = Arc::new(McpClient::new(McpClientOptions {
            request_timeout_ms: entry.config.timeout_ms(),
            ..Default::default()
        }));
        {
            let mut clients = self.clients.lock().expect("clients lock");
            if self.cancel.is_cancelled() {
                return Err("MCP connection cancelled".to_owned());
            }
            clients.push(client.clone());
        }
        let mut notifications = client.subscribe_notifications();
        let connected = async {
            let initialized = client.connect(transport).await?;
            let tools = if initialized.capabilities.tools.is_some() {
                client.list_tools(McpRequestOptions::default()).await?
            } else {
                Vec::new()
            };
            Ok::<_, String>(PreconnectedMcpServer {
                name: entry.name.clone(),
                exposure: entry.config.exposure(),
                tool_exposure: entry.config.tool_exposure().clone(),
                timeout_ms: entry.config.timeout_ms(),
                client: client.clone(),
                tools,
                has_resources: initialized.capabilities.resources.is_some(),
            })
        }
        .await;
        let server = match connected {
            Ok(server) => server,
            Err(error) => {
                let _ = client.close().await;
                return Err(error);
            }
        };
        {
            let _publish = self.publish.lock().await;
            runtime.check_active()?;
            if self.cancel.is_cancelled() {
                return Err("MCP connection cancelled".to_owned());
            }
            self.servers.lock().expect("servers lock").push(server);
            self.publish_tools(&runtime)?;
        }
        let weak = Arc::downgrade(self);
        let cancel = self.cancel.clone();
        tokio::spawn(async move {
            loop {
                let refresh = tokio::select! {
                    biased; () = cancel.cancelled() => break,
                    notification = notifications.recv() => match notification {
                        Ok((method, _)) => matches!(method.as_str(), "notifications/tools/list_changed" | "notifications/resources/list_changed"),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => true,
                        Err(_) => break,
                    }
                };
                if !refresh {
                    continue;
                }
                let Some(state) = weak.upgrade() else {
                    break;
                };
                let refresh_tools = async {
                    if client
                        .server_capabilities()
                        .is_some_and(|caps| caps.tools.is_some())
                    {
                        client.list_tools(McpRequestOptions::default()).await
                    } else {
                        Ok(Vec::new())
                    }
                };
                let result = tokio::select! { biased; () = cancel.cancelled() => break, result = refresh_tools => result };
                match result {
                    Ok(tools) => {
                        let _publish = state.publish.lock().await;
                        if cancel.is_cancelled() || runtime.check_active().is_err() {
                            break;
                        }
                        if let Some(server) = state
                            .servers
                            .lock()
                            .expect("servers lock")
                            .iter_mut()
                            .find(|server| server.name == entry.name)
                        {
                            server.tools = tools;
                            server.has_resources = client
                                .server_capabilities()
                                .is_some_and(|caps| caps.resources.is_some());
                        }
                        if let Err(error) = state.publish_tools(&runtime) {
                            state
                                .errors
                                .lock()
                                .expect("errors lock")
                                .insert(entry.name.clone(), error);
                        }
                    }
                    Err(error) => {
                        state
                            .errors
                            .lock()
                            .expect("errors lock")
                            .insert(entry.name.clone(), error);
                    }
                }
            }
        });
        Ok(())
    }

    fn publish_tools(&self, runtime: &ExtensionRuntime) -> Result<(), String> {
        let definitions = mcp_tool_definitions(&self.servers.lock().expect("servers lock")).0;
        runtime.replace_extension_tools("builtin:mcp", definitions)?;
        self.activate_helpers(runtime)
    }

    async fn wait(&self, direct_only: bool, cancellation: Option<CancellationToken>) {
        let relevant: Vec<_> = self
            .config
            .servers
            .iter()
            .filter(|entry| {
                !direct_only
                    || entry.config.exposure() == McpExposure::Direct
                    || entry
                        .config
                        .tool_exposure()
                        .values()
                        .any(|exposure| *exposure == McpExposure::Direct)
            })
            .collect();
        let maximum = relevant
            .iter()
            .map(|entry| entry.config.startup_wait_ms())
            .max()
            .unwrap_or(0);
        let mut changed = self.changed.subscribe();
        let cancel = cancellation.unwrap_or_default();
        let wait = async {
            loop {
                if !relevant.iter().any(|entry| {
                    self.pending
                        .lock()
                        .expect("pending lock")
                        .contains(&entry.name)
                }) {
                    break;
                }
                if changed.changed().await.is_err() {
                    break;
                }
            }
        };
        tokio::select! { biased; () = self.cancel.cancelled() => {}, () = cancel.cancelled() => {}, _ = tokio::time::timeout(Duration::from_millis(maximum), wait) => {} }
    }

    async fn close(&self) {
        self.cancel.cancel();
        let _publish = self.publish.lock().await;
        let clients = std::mem::take(&mut *self.clients.lock().expect("clients lock"));
        futures_util::future::join_all(clients.into_iter().map(|client| async move {
            let _ = client.close().await;
        }))
        .await;
    }
}

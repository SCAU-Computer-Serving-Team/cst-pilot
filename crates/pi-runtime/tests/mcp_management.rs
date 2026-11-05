#[path = "support/mcp_http.rs"]
mod fixture;

use fixture::{Response, Server, initialize};
use pi_runtime::{
    CreateAgentSessionOptions, add_mcp_server_config, create_agent_session,
    load_mcp_config_with_registered, remove_mcp_server_config, update_mcp_server_config,
    validate_mcp_server_config,
};
use pi_session::{
    AgentSession, ExtensionInitError, InlineExtension, RegisteredMcpServer, SourceInfo,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

struct Directory(std::path::PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("pi-mcp-management-{}", pi_ai::uuid_v7()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn wait_for(session: &AgentSession, name: &str, present: bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if session.get_all_tools().iter().any(|tool| tool.name == name) == present {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn command(session: &AgentSession, args: &str) -> Result<(), String> {
    let runner = session.extension_runner();
    let command = runner.get_command("mcp").unwrap();
    (command.command.handler)(args.to_owned(), runner.create_command_context()).await
}

#[test]
fn config_transactions_preserve_unknown_fields_reject_corruption_and_serialize_writers() {
    let dir = Directory::new();
    let path = dir.0.join("mcp.json");
    std::fs::write(
        &path,
        r#"{"custom":{"keep":true},"mcpServers":{"existing":{"command":"echo","future":42}}}"#,
    )
    .unwrap();
    let config = validate_mcp_server_config("new", &json!({"command":"echo"})).unwrap();
    std::thread::scope(|scope| {
        for index in 0..12 {
            let path = &path;
            let config = &config;
            scope.spawn(move || {
                add_mcp_server_config(path, &format!("server{index}"), config).unwrap()
            });
        }
    });
    update_mcp_server_config(
        &path,
        "existing",
        &json!({"exposure":"direct","enabled":false}),
    )
    .unwrap();
    let value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(value["custom"]["keep"], true);
    assert_eq!(value["mcpServers"]["existing"]["future"], 42);
    assert_eq!(value["mcpServers"].as_object().unwrap().len(), 13);
    assert!(update_mcp_server_config(&path, "existing", &json!({"command":true})).is_err());
    add_mcp_server_config(&path, "same-name", &config).unwrap();
    assert!(add_mcp_server_config(&path, "same_name", &config).is_err());
    remove_mcp_server_config(&path, "same-name").unwrap();
    std::fs::write(&path, "{bad json").unwrap();
    assert!(add_mcp_server_config(&path, "new", &config).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{bad json");
}

#[test]
fn registered_servers_are_defaults_and_project_overrides_require_trust() {
    let dir = Directory::new();
    let agent = dir.0.join("agent");
    let project = dir.0.join(".pi");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let registered = vec![RegisteredMcpServer {
        name: "docs".to_owned(),
        config: validate_mcp_server_config("docs", &json!({"command":"extension"})).unwrap(),
        source_info: SourceInfo::synthetic("test", "extension"),
    }];
    std::fs::write(
        project.join("mcp.json"),
        r#"{"mcpServers":{"docs":{"exposure":"direct"}}}"#,
    )
    .unwrap();
    let trusted = load_mcp_config_with_registered(&agent, &dir.0, true, &registered);
    assert!(trusted.errors.is_empty());
    assert_eq!(
        trusted.servers[0].config.exposure(),
        pi_runtime::McpExposure::Direct
    );
    let untrusted = load_mcp_config_with_registered(&agent, &dir.0, false, &registered);
    assert_eq!(
        untrusted.servers[0].config.exposure(),
        pi_runtime::McpExposure::Codemode
    );
    std::fs::write(
        agent.join("mcp.json"),
        r#"{"mcpServers":{"docs":{"command":"disk"}}}"#,
    )
    .unwrap();
    let loaded = load_mcp_config_with_registered(&agent, &dir.0, false, &registered);
    let pi_runtime::McpServerConfig::Stdio(config) = &loaded.servers[0].config else {
        panic!()
    };
    assert_eq!(config.command, "disk");
}

#[tokio::test]
async fn extension_registration_live_inventory_and_management_reconnect_cleanly() {
    let dir = Directory::new();
    let connects = Arc::new(AtomicUsize::new(0));
    let lists = Arc::new(AtomicUsize::new(0));
    let server = Server::spawn({ let connects = connects.clone(); let lists = lists.clone(); move |request| { let connects = connects.clone(); let lists = lists.clone(); async move {
        if request.method == "GET" { return Response::sse("data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n\n".to_owned()); }
        let message = request.json();
        let result = match message["method"].as_str() {
            Some("initialize") => { connects.fetch_add(1, Ordering::SeqCst); initialize("1") }
            Some("tools/list") => { let n = lists.fetch_add(1, Ordering::SeqCst); json!({"tools":[{"name":if n == 0 {"old"} else {"current"},"description":"Search docs","inputSchema":{"type":"object"}}]}) }
            _ => return Response::status(202),
        };
        Response::json(json!({"jsonrpc":"2.0","id":message["id"],"result":result}))
    }}}).await;
    let config = validate_mcp_server_config(
        "docs",
        &json!({"url":format!("{}/mcp", server.url),"exposure":"direct"}),
    )
    .unwrap();
    let extension_config = config.clone();
    let factory: pi_session::ExtensionFactory = Arc::new(move || {
        let config = extension_config.clone();
        Ok(Box::new(InlineExtension::new("test:mcp", move |api| {
            api.register_mcp_server("docs", config.clone())
                .map_err(|message| ExtensionInitError {
                    extension_path: "test:mcp".to_owned(),
                    message,
                })?;
            Ok(())
        })))
    });
    let created = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(dir.0.clone()),
        agent_dir: Some(dir.0.join("agent")),
        extension_factories: vec![factory],
        ..Default::default()
    })
    .await
    .unwrap();
    let session = created.session;
    wait_for(&session, "mcp__docs__current", true).await;
    assert!(
        !session
            .get_all_tools()
            .iter()
            .any(|tool| tool.name == "mcp__docs__old")
    );
    assert!(
        session
            .get_active_tool_names()
            .contains(&"mcp__docs__current".to_owned())
    );
    assert!(command(&session, "reconnect missing").await.is_err());
    assert!(command(&session, "unknown").await.is_err());
    let runtime = session.extension_runner().runtime();
    assert_eq!(runtime.get_mcp_servers().unwrap().len(), 1);
    runtime
        .register_mcp_server(
            "test:mcp",
            "docs",
            validate_mcp_server_config(
                "docs",
                &json!({"url":format!("{}/mcp",server.url),"enabled":false}),
            )
            .unwrap(),
        )
        .unwrap();
    wait_for(&session, "mcp__docs__current", false).await;
    runtime
        .register_mcp_server("test:mcp", "docs", config)
        .unwrap();
    wait_for(&session, "mcp__docs__current", true).await;
    assert!(connects.load(Ordering::SeqCst) >= 2);
    command(
        &session,
        "update   global   docs   {\"exposure\":\"hidden\"}",
    )
    .await
    .unwrap();
    wait_for(&session, "mcp__docs__current", false).await;
    let stored: Value =
        serde_json::from_slice(&std::fs::read(dir.0.join("agent/mcp.json")).unwrap()).unwrap();
    assert_eq!(stored["mcpServers"]["docs"], json!({"exposure":"hidden"}));
    command(&session, "update global docs {\"exposure\":\"direct\"}")
        .await
        .unwrap();
    wait_for(&session, "mcp__docs__current", true).await;
    command(&session, "remove global docs").await.unwrap();
    assert!(session.new_session(None).await.unwrap());
    wait_for(&session, "mcp__docs__current", true).await;
    let runtime = session.extension_runner().runtime();
    runtime.unregister_mcp_server("test:mcp", "docs").unwrap();
    wait_for(&session, "mcp__docs__current", false).await;
    command(
        &session,
        &format!(
            "add global local {{\"url\":\"{}/mcp\",\"exposure\":\"direct\"}}",
            server.url
        ),
    )
    .await
    .unwrap();
    wait_for(&session, "mcp__local__current", true).await;
    command(
        &session,
        "update global local {\"enabled\":false,\"exposure\":\"hidden\"}",
    )
    .await
    .unwrap();
    wait_for(&session, "mcp__local__current", false).await;
    command(&session, "remove global local").await.unwrap();
    session.shutdown().await.unwrap();
}

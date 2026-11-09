use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cst_pilot::tools::fff::{SearchInput as FileSearchInput, search};
use cst_pilot::tools::path_guard::{GuardedReadOperations, PathGuard};
use cst_pilot::tools::ssrf::{
    DnsResolver, HttpRequest, HttpResponse, HttpTransport, NetworkPolicy, SafeHttpClient,
};
use cst_pilot::tools::web_access::{ContentInput, FetchInput, SearchInput, WebAccess};
use cst_pilot::{CstPilotExtension, Kit, OPEN_TOOL_NAMES, SKILL_NAMES};
use pi_agent::BoxFuture;
use pi_ai::faux::{
    create_faux_provider, faux_assistant_message, faux_text_message, faux_tool_call,
};
use pi_ai::{StopReason, UserContentBlock};
use pi_runtime::{
    CreateAgentSessionOptions, CreateModelRuntimeOptions, ModelRuntime, ReadOperations,
    create_agent_session,
};
use pi_session::SessionManager;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("cst-stage6-security-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let fixture = Self(root.canonicalize().unwrap());
        for name in SKILL_NAMES {
            fixture.write(
                &format!("agent/home/skills/{name}/SKILL.md"),
                &format!("---\nname: {name}\ndescription: {name} 工具指南\n---\n维护指南。"),
            );
        }
        fixture.write("agent/home/APPEND_SYSTEM.md", "CST Pilot 固定诊断规则。");
        fixture
    }
    fn path(&self, path: &str) -> PathBuf {
        self.0.join(path)
    }
    fn write(&self, path: &str, text: &str) {
        let path = self.path(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    fn guard(&self) -> PathGuard {
        PathGuard::new(&self.0).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn file_input(pattern: &str) -> FileSearchInput {
    serde_json::from_value(json!({"pattern":pattern})).unwrap()
}
fn query_input(query: &str) -> SearchInput {
    serde_json::from_value(json!({"query":query})).unwrap()
}
fn content_input(id: &str) -> ContentInput {
    serde_json::from_value(json!({"responseId":id})).unwrap()
}

#[test]
fn allowlist_has_exactly_the_product_tools_without_execution_or_writes() {
    let expected = [
        "read",
        "ls",
        "disk",
        "sys",
        "startup",
        "eventlog",
        "driver",
        "command_plan",
        "runbook_deliver",
        "runbook",
        "bitlocker",
        "fffind",
        "ffgrep",
        "web_search",
        "source_check",
        "fetch_content",
        "get_search_content",
    ];
    assert_eq!(OPEN_TOOL_NAMES.len(), 17);
    assert_eq!(
        OPEN_TOOL_NAMES.iter().copied().collect::<BTreeSet<_>>(),
        expected.into_iter().collect()
    );
    let fixture = Fixture::new();
    let extension = CstPilotExtension::new(&Kit::new(&fixture.0).unwrap()).unwrap();
    assert_eq!(extension.tool_names().len(), OPEN_TOOL_NAMES.len());
    assert_eq!(
        extension
            .tool_names()
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        OPEN_TOOL_NAMES.iter().copied().collect()
    );
}

#[test]
fn every_credential_and_state_path_is_blocked_but_normal_documents_work() {
    let fixture = Fixture::new();
    let guard = fixture.guard();
    for path in [
        "agent/home/auth.json",
        "agent/home/models.json",
        "agent/home/mcp-auth.json",
        "agent/home/web-search.json",
        "agent/home/telemetry/identity.json",
        ".state/secret.txt",
    ] {
        fixture.write(path, "synthetic-test-secret");
        assert!(guard.check(fixture.path(path)).is_err(), "{path}");
        let ops = GuardedReadOperations(guard.clone());
        assert!(ops.access(&fixture.path(path)).is_err());
        assert!(ops.detect_image_mime_type(&fixture.path(path)).is_err());
        assert!(ops.read_bytes(&fixture.path(path)).is_err());
        assert!(ops.open_reader(&fixture.path(path)).is_err());
    }
    assert!(guard.check(fixture.path(".state")).is_err());
    fixture.write("notes/readme.md", "可公开的诊断内容");
    assert_eq!(
        GuardedReadOperations(guard)
            .read_bytes(&fixture.path("notes/readme.md"))
            .unwrap(),
        "可公开的诊断内容".as_bytes()
    );
}

#[test]
fn traversal_and_hard_link_aliases_cannot_read_credentials() {
    let fixture = Fixture::new();
    fixture.write("agent/home/auth.json", "synthetic-test-secret");
    fixture.write("normal.txt", "normal");
    std::fs::hard_link(
        fixture.path("agent/home/auth.json"),
        fixture.path("innocent.txt"),
    )
    .unwrap();
    assert!(fixture.guard().open(fixture.path("innocent.txt")).is_err());
    assert!(
        fixture
            .guard()
            .open(fixture.path("agent/home/skills/../auth.json"))
            .is_err()
    );
    #[cfg(windows)]
    {
        assert!(
            fixture
                .guard()
                .check(fixture.path("AGENT/HOME/AUTH.JSON"))
                .is_err()
        );
        assert!(
            fixture
                .guard()
                .check(fixture.path("agent/home/auth.json::$DATA"))
                .is_err()
        );
        assert!(
            fixture
                .guard()
                .check(fixture.path("agent/home/auth.json."))
                .is_err()
        );
    }
}

#[cfg(unix)]
#[test]
fn symbolic_links_to_credentials_and_state_are_blocked() {
    let fixture = Fixture::new();
    fixture.write("agent/home/auth.json", "secret");
    fixture.write(".state/task.txt", "state");
    std::os::unix::fs::symlink(
        fixture.path("agent/home/auth.json"),
        fixture.path("alias.txt"),
    )
    .unwrap();
    std::os::unix::fs::symlink(fixture.path(".state"), fixture.path("state-alias")).unwrap();
    assert!(fixture.guard().open(fixture.path("alias.txt")).is_err());
    assert!(
        fixture
            .guard()
            .check(fixture.path("state-alias/task.txt"))
            .is_err()
    );
}

#[test]
fn native_search_honors_ignores_filters_context_and_secret_exclusion() {
    let fixture = Fixture::new();
    fixture.write(".gitignore", "ignored/\n");
    fixture.write("ignored/private.txt", "needle");
    fixture.write("src/power_status.rs", "before\nneedle\nafter\n");
    fixture.write("src/other.rs", "other\n");
    fixture.write("agent/home/auth.json", "needle-secret");
    fixture.write(".state/log.txt", "needle-state");
    let guard = fixture.guard();
    let cancel = CancellationToken::new();
    let found = search(&guard, &fixture.0, file_input("pwr sts"), false, &cancel).unwrap();
    assert_eq!(found["matches"][0]["path"], "src/power_status.rs");
    let input =
        serde_json::from_value(json!({"pattern":"needle","path":"src/*.rs","context":1})).unwrap();
    let found = search(&guard, &fixture.0, input, true, &cancel).unwrap();
    assert_eq!(found["matches"].as_array().unwrap().len(), 3);
    assert_eq!(found["matches"][1]["line"], 2);
    let found = search(&guard, &fixture.0, file_input("needle"), true, &cancel).unwrap();
    assert_eq!(found["matches"].as_array().unwrap().len(), 1);
    assert!(!found.to_string().contains("secret"));
    let all = search(&guard, &fixture.0, file_input(""), false, &cancel).unwrap();
    assert!(!all.to_string().contains("auth.json"));
    assert!(!all.to_string().contains("ignored"));
    assert!(!all.to_string().contains(".state"));
}

#[test]
fn search_bounds_long_lines_output_and_respects_cancellation() {
    let fixture = Fixture::new();
    fixture.write("huge.txt", &format!("{}\nneedle", "x".repeat(1024 * 1024)));
    let result = search(
        &fixture.guard(),
        &fixture.0,
        file_input("needle"),
        true,
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(result["matches"][0]["line"], 2);
    assert_eq!(result["skipped"], 1);
    for i in 0..100 {
        fixture.write(&format!("data/{i}.txt"), "needle");
    }
    let input = serde_json::from_value(json!({"pattern":"needle","limit":2})).unwrap();
    let result = search(
        &fixture.guard(),
        &fixture.0,
        input,
        true,
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(result["matches"].as_array().unwrap().len(), 2);
    assert_eq!(result["truncated"], true);
    assert!(result.to_string().len() < 50 * 1024);
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        search(
            &fixture.guard(),
            &fixture.0,
            file_input("needle"),
            true,
            &cancelled
        )
        .is_err()
    );
    assert!(
        search(
            &fixture.guard(),
            &fixture.0,
            file_input("["),
            true,
            &CancellationToken::new()
        )
        .is_err()
    );
}

#[test]
fn ssrf_blocks_private_special_ipv6_and_mapped_addresses_and_limits_tun_exception() {
    let policy = NetworkPolicy::default();
    for value in [
        "0.0.0.0",
        "10.0.0.1",
        "127.0.0.1",
        "100.64.0.1",
        "169.254.169.254",
        "172.16.0.1",
        "192.168.0.1",
        "198.18.0.1",
        "198.19.255.255",
        "192.0.2.1",
        "224.0.0.1",
        "255.255.255.255",
        "::",
        "::1",
        "fc00::1",
        "fe80::1",
        "ff02::1",
        "::ffff:127.0.0.1",
        "2001:db8::1",
        "2002:7f00:1::1",
        "64:ff9b::7f00:1",
    ] {
        assert!(
            policy.validate_ip(value.parse().unwrap()).is_err(),
            "{value}"
        );
    }
    for value in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
        assert!(
            policy.validate_ip(value.parse().unwrap()).is_ok(),
            "{value}"
        );
    }
    let tun = NetworkPolicy { allow_tun: true };
    assert!(tun.validate_ip("198.18.0.1".parse().unwrap()).is_ok());
    assert!(tun.validate_ip("198.19.255.255".parse().unwrap()).is_ok());
    assert!(tun.validate_ip("10.0.0.1".parse().unwrap()).is_err());
    for value in [
        "file:///C:/Windows/a",
        "http://user:pass@example.com",
        "http://localhost",
        "http://name.local",
        "http://127.1",
        "http://2130706433",
        "http://[::1]",
    ] {
        assert!(policy.validate_url(value).is_err(), "{value}");
    }
}

struct Dns(Vec<SocketAddr>);
impl DnsResolver for Dns {
    fn resolve(&self, _: String, _: u16) -> BoxFuture<'_, Result<Vec<SocketAddr>, String>> {
        Box::pin(async { Ok(self.0.clone()) })
    }
}

#[tokio::test]
async fn dns_validation_rejects_mixed_answers_and_pins_only_validated_addresses() {
    let client = SafeHttpClient::with_resolver(
        NetworkPolicy::default(),
        Arc::new(Dns(vec![
            "8.8.8.8:443".parse().unwrap(),
            "127.0.0.1:443".parse().unwrap(),
        ])),
    );
    assert!(
        client
            .resolve_target("https://public.example")
            .await
            .is_err()
    );
    let client = SafeHttpClient::with_resolver(
        NetworkPolicy::default(),
        Arc::new(Dns(vec!["8.8.8.8:443".parse().unwrap()])),
    );
    let (_, addresses) = client
        .resolve_target("https://public.example")
        .await
        .unwrap();
    assert_eq!(
        addresses,
        vec!["8.8.8.8:443".parse::<SocketAddr>().unwrap()]
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(
        client
            .request(
                HttpRequest {
                    url: "https://public.example".into(),
                    body: None,
                    headers: vec![]
                },
                cancel
            )
            .await
            .is_err()
    );
}

#[derive(Default)]
struct MockWeb {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<std::collections::VecDeque<HttpResponse>>,
}
impl HttpTransport for MockWeb {
    fn request(
        &self,
        request: HttpRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<HttpResponse, String>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err("cancelled".into());
            }
            self.requests.lock().unwrap().push(request);
            Ok(self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(HttpResponse {
                    status: 200,
                    body: "网页正文".into(),
                }))
        })
    }
}
impl MockWeb {
    fn reply(&self, body: Value) {
        self.replies.lock().unwrap().push_back(HttpResponse {
            status: 200,
            body: body.to_string(),
        });
    }
}

#[tokio::test]
async fn exa_mcp_search_caches_pages_and_finds_chinese_without_network() {
    let transport = Arc::new(MockWeb::default());
    transport.reply(json!({"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"Title: 维修指南\nURL: https://example.com/guide\n正文 中文字符"}]}}));
    let web = WebAccess::with_transport(transport.clone());
    let result = web
        .search(query_input("Windows 诊断"), CancellationToken::new())
        .await
        .unwrap();
    let id = result["responseId"].as_str().unwrap();
    let request = &transport.requests.lock().unwrap()[0];
    assert_eq!(request.url, "https://mcp.exa.ai/mcp");
    assert_eq!(
        request.body.as_ref().unwrap()["params"]["name"],
        "web_search_exa"
    );
    let all = web.get(content_input(id)).unwrap();
    assert!(all["content"].as_str().unwrap().contains("维修指南"));
    let found = web
        .get(serde_json::from_value(json!({"responseId":id,"findText":"中文字符"})).unwrap())
        .unwrap();
    assert!(found["content"].as_str().unwrap().contains("中文字符"));
    let page = web
        .get(serde_json::from_value(json!({"responseId":id,"offset":3,"limit":2})).unwrap())
        .unwrap();
    assert_eq!(page["content"].as_str().unwrap().chars().count(), 2);
    assert!(
        WebAccess::with_transport(Arc::new(MockWeb::default()))
            .get(content_input(id))
            .is_err()
    );
}

#[tokio::test]
async fn fetch_strips_scripts_and_cache_evicts_old_results_without_disk_writes() {
    let transport = Arc::new(MockWeb::default());
    transport.replies.lock().unwrap().push_back(HttpResponse {
        status: 200,
        body: "<html><script>hidden()</script><p>内容 &amp; 数据</p></html>".into(),
    });
    let web = WebAccess::with_transport(transport);
    let result = web
        .fetch(
            FetchInput {
                url: Some("https://example.com".into()),
                urls: vec![],
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let first = result["responseId"].as_str().unwrap();
    let content = web.get(content_input(first)).unwrap();
    assert!(content["content"].as_str().unwrap().contains("内容 & 数据"));
    assert!(!content["content"].as_str().unwrap().contains("hidden()"));
    for _ in 0..32 {
        web.fetch(
            FetchInput {
                url: Some("https://example.com".into()),
                urls: vec![],
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    }
    assert!(web.get(content_input(first)).is_err());
}

#[tokio::test]
async fn errors_and_invalid_requests_do_not_leak_provider_response_bodies() {
    let transport = Arc::new(MockWeb::default());
    transport.replies.lock().unwrap().push_back(HttpResponse {
        status: 401,
        body: "sensitive-provider-message".into(),
    });
    let web = WebAccess::with_transport(transport);
    let error = web
        .search(query_input("a"), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(!error.contains("sensitive-provider-message"));
    assert!(
        web.search(query_input(""), CancellationToken::new())
            .await
            .is_err()
    );
    assert!(
        web.fetch(
            FetchInput {
                url: Some("http://127.0.0.1".into()),
                urls: vec![]
            },
            CancellationToken::new()
        )
        .await
        .is_err()
    );
    let fixture = Fixture::new();
    fixture.write("agent/home/web-search.json", "{invalid-sensitive-config");
    assert!(
        !CstPilotExtension::new(&Kit::new(&fixture.0).unwrap())
            .err()
            .unwrap()
            .contains("sensitive-config")
    );
}

#[tokio::test]
async fn sdk_session_keeps_resources_and_allowlist_isolated_through_reload_and_tool_changes() {
    let fixture = Fixture::new();
    fixture.write("host/AGENTS.md", "HOST_CONTEXT_MUST_NOT_LOAD");
    fixture.write("host/.pi/SYSTEM.md", "HOST_SYSTEM_MUST_NOT_LOAD");
    fixture.write(
        "host/.pi/skills/host/SKILL.md",
        "---\nname: host\ndescription: hostile\n---\nHOST_SKILL_MUST_NOT_LOAD",
    );
    fixture.write(
        "host/.pi/settings.json",
        r#"{"defaultProvider":"hostile","defaultTools":["bash"],"defaultProjectTrust":"always"}"#,
    );
    fixture.write(
        "agent/home/settings.json",
        r#"{"defaultProjectTrust":"always"}"#,
    );
    fixture.write("host/normal.txt", "可公开的文件内容");
    fixture.write("agent/home/auth.json", "SYNTHETIC_AUTH_SECRET");
    let faux = create_faux_provider();
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        include_builtins: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await;
    runtime.register_provider(faux.provider.clone());
    let cwd = fixture.path("host");
    let options = Kit::new(&fixture.0)
        .unwrap()
        .session_options(CreateAgentSessionOptions {
            cwd: Some(cwd.clone()),
            model_runtime: Some(runtime),
            model: Some(faux.model.clone()),
            session_manager: Some(
                SessionManager::in_memory(cwd.to_string_lossy(), None, None).unwrap(),
            ),
            tools: Some(vec!["*".into()]),
            ..Default::default()
        })
        .unwrap();
    let created = create_agent_session(options).await.unwrap();
    let expected = CstPilotExtension::new(&Kit::new(&fixture.0).unwrap())
        .unwrap()
        .tool_names()
        .into_iter()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        created
            .session
            .get_active_tool_names()
            .into_iter()
            .collect::<BTreeSet<_>>(),
        expected
    );
    assert_eq!(created.resource_loader.get_skills().skills.len(), 8);
    assert!(
        created
            .resource_loader
            .get_skills()
            .skills
            .iter()
            .all(|skill| SKILL_NAMES.contains(&skill.name.as_str()))
    );
    assert!(created.resource_loader.get_agents_files().is_empty());
    assert!(created.resource_loader.get_system_prompt().is_none());
    assert_eq!(
        created.resource_loader.get_append_system_prompt(),
        ["CST Pilot 固定诊断规则。"]
    );
    assert_eq!(created.settings_manager.get_default_provider(), None);
    assert!(!created.settings_manager.is_project_trusted());
    created
        .session
        .set_active_tools_by_name(vec![
            "bash".into(),
            "powershell".into(),
            "write".into(),
            "ls".into(),
            "read".into(),
        ])
        .unwrap();
    assert_eq!(created.session.get_active_tool_names(), ["read", "ls"]);
    faux.set_responses([
        faux_assistant_message(
            vec![faux_tool_call(
                "read",
                json!({"path":"normal.txt"}).as_object().unwrap().clone(),
                Some("normal".into()),
            )],
            Some(StopReason::ToolUse),
        )
        .into(),
        faux_text_message("读取完成。").into(),
    ]);
    created
        .session
        .prompt("读取 normal.txt", None)
        .await
        .unwrap();
    let tools = created.session.get_callable_tools();
    assert!(
        tools
            .iter()
            .find(|tool| tool.name == "ls")
            .unwrap()
            .description
            .contains("按大小")
    );
    let read = tools.iter().find(|tool| tool.name == "read").unwrap();
    let error = (read.execute)(
        "denied".into(),
        json!({"path":"../agent/home/auth.json"}),
        None,
        None,
    )
    .await
    .unwrap_err();
    assert!(!error.contains("SYNTHETIC_AUTH_SECRET"));
    let result = (read.execute)("normal".into(), json!({"path":"normal.txt"}), None, None)
        .await
        .unwrap();
    assert!(result.content.iter().any(|block| matches!(block, UserContentBlock::Text(text) if text.text.contains("可公开的文件内容"))));
    created.session.reload().await.unwrap();
    assert!(
        created
            .session
            .get_active_tool_names()
            .iter()
            .all(|name| expected.contains(name))
    );
    created.session.shutdown().await.unwrap();
}

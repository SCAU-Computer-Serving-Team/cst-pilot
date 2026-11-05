use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_agent::{AgentTool, AgentToolResult};
use pi_ai::{JsonValue, UserContentBlock};
use pi_runtime::{
    Bm25Ranker, CODEMODE_STORE_ENTRY_TYPE, CODEMODE_TOOL_NAME, CallToolResult,
    CodemodeDeclarationItem, CodemodeDeclaredTool, CodemodeDescriptionOptions, CodemodeErrorKind,
    CodemodeExecuteOptions, CodemodeExtensionOptions, CodemodeOutcome, CodemodeOutputItem,
    CodemodeRunOptions, CodemodeSandbox, CodemodeSandboxOptions, CodemodeTool,
    CreateAgentSessionOptions, InMemoryTransport, LIST_MCP_RESOURCES_TOOL, McpClient,
    McpClientOptions, McpContentBlock, McpExposure, McpExtensionOptions, McpRequestOptions,
    McpServerConfig, McpToolAnnotations, McpTransport, PreconnectedMcpServer,
    READ_MCP_RESOURCE_TOOL, RenderDeclarationsOptions, SchemaToTypeOptions, TOOL_SEARCH_TOOL_NAME,
    ToolRanker, create_agent_session, create_codemode_description, create_codemode_extension,
    create_mcp_extension, create_mcp_result_schema, create_mcp_tool_name,
    create_tool_search_document, create_tool_search_extension, get_mcp_tool_exposure,
    limit_mcp_content, load_mcp_config, parse_codemode_source, render_declarations,
    render_tool_sample, render_tool_signature, schema_to_type, tokenize,
    validate_mcp_server_config,
};
use pi_session::{
    ExtensionFactory, InlineExtension, NestedToolCallOptions, SessionManager, ToolDefinition,
    ToolNamespace,
};
use serde_json::json;

fn stub_agent_tool(name: &str, description: &str, properties: JsonValue) -> AgentTool {
    AgentTool::new(
        name.to_owned(),
        name.to_owned(),
        description.to_owned(),
        json!({
            "type": "object",
            "properties": properties,
        }),
        Arc::new(|_, _, _, _| {
            Box::pin(async move {
                Ok(AgentToolResult {
                    content: Vec::new(),
                    details: JsonValue::Null,
                    structured_content: None,
                    usage: None,
                    is_error: false,
                    terminate: None,
                })
            })
        }),
    )
}

#[test]
fn tool_search_tokenize_and_bm25_ranker_match_upstream_semantics() {
    assert_eq!(
        tokenize("listIssues for the GitHub_repo"),
        vec!["list", "issue", "git", "hub", "repo"]
    );
    assert_eq!(
        tokenize("searches queries HTTPServer"),
        vec!["search", "query", "http", "server"]
    );
    assert_eq!(
        tokenize("repo.issues/search:queries"),
        vec!["repo", "issue", "search", "query"]
    );
    assert_eq!(tokenize("CAFÉ cafe\u{0301}"), vec!["café", "café"]);
    assert!(tokenize(" \t，。!? 😀 ").is_empty());

    let tools = [
        stub_agent_tool(
            "mcp__github__list_issues",
            "List issues in a repository.",
            json!({ "state": { "type": "string", "description": "open or closed" } }),
        ),
        stub_agent_tool(
            "mcp__github__create_pull_request",
            "Open a pull request.",
            json!({}),
        ),
        stub_agent_tool(
            "mcp__linear__search_issues",
            "Search Linear issues by text.",
            json!({}),
        ),
        stub_agent_tool("mcp__docs__search", "Search the documentation.", json!({})),
    ];
    let documents: Vec<_> = tools
        .iter()
        .map(|t| create_tool_search_document(&t.name, &t.description, t.parameters(), None))
        .collect();

    let ranker = Bm25Ranker::default();
    let issue_names: Vec<_> = ranker
        .rank("issue", &documents, 8)
        .into_iter()
        .map(|m| m.name)
        .collect();
    assert_eq!(
        issue_names,
        vec!["mcp__linear__search_issues", "mcp__github__list_issues"]
    );
    assert_eq!(
        ranker.rank("pull requests", &documents, 8)[0].name,
        "mcp__github__create_pull_request"
    );
    assert_eq!(ranker.rank("search", &documents, 1).len(), 1);
    assert_eq!(
        ranker
            .rank("closed", &documents, 8)
            .into_iter()
            .map(|m| m.name)
            .collect::<Vec<_>>(),
        vec!["mcp__github__list_issues"]
    );

    for (description, query) in [
        ("搜索仓库问题", "问题"),
        ("文書を検索", "検索"),
        ("문서 검색", "검색"),
        ("ค้นหาเอกสาร", "เอกสาร"),
        ("Поиск документов", "документов"),
        ("البحث عن الملفات", "الملفات"),
        ("फ़ाइल खोजें", "खोजें"),
        ("Rechercher un café", "cafe\u{0301}"),
    ] {
        let localized = create_tool_search_document("localized", description, &json!({}), None);
        let mut docs = documents.clone();
        docs.push(localized);
        let matches = ranker.rank(query, &docs, 8);
        assert_eq!(matches.len(), 1, "failed query {query} on {description}");
        assert_eq!(matches[0].name, "localized");
    }

    assert!(ranker.rank("kubernetes", &documents, 8).is_empty());
    assert!(ranker.rank("the", &documents, 8).is_empty());

    // Exact name priority (#11331)
    let named = vec![
        create_tool_search_document(
            "mcp__app__queue_read",
            "Read a queued message of this thread.",
            &json!({}),
            None,
        ),
        create_tool_search_document(
            "mcp__dev-app__thread_read",
            "Read durable state and a paginated timeline from any thread. The default view returns user messages, assistant messages and plans. Continue with afterPosition until hasMore is false.",
            &json!({}),
            None,
        ),
    ];
    for q in [
        "thread_read",
        "dev_app__thread_read",
        "mcp__dev-app__thread_read",
        "Thread_Read",
        "thread read",
    ] {
        assert_eq!(
            ranker
                .rank(q, &named, 1)
                .into_iter()
                .map(|m| m.name)
                .collect::<Vec<_>>(),
            vec!["mcp__dev-app__thread_read"]
        );
    }

    // Whole local name bonus & coverage priority
    let peekaboo_ns = ToolNamespace {
        name: "mcp__peekaboo".to_owned(),
        aliases: Vec::new(),
        description: None,
        instructions: Some("Peekaboo controls macOS apps by app name.".to_owned()),
    };
    let peekaboo_docs = vec![
        create_tool_search_document(
            "mcp__peekaboo__see",
            "Capture the app window of the peekaboo app and describe the app.",
            &json!({}),
            Some(&peekaboo_ns),
        ),
        create_tool_search_document(
            "mcp__peekaboo__app",
            "Launch, quit, relaunch, focus, hide, or switch applications.",
            &json!({}),
            Some(&peekaboo_ns),
        ),
        create_tool_search_document(
            "mcp__peekaboo__window",
            "Move, resize, or close a window of an app.",
            &json!({}),
            Some(&peekaboo_ns),
        ),
        create_tool_search_document(
            "mcp__peekaboo__image",
            "Take a screenshot of the screen, an app, or a window.",
            &json!({}),
            Some(&peekaboo_ns),
        ),
    ];
    assert_eq!(
        ranker.rank("peekaboo app", &peekaboo_docs, 1)[0].name,
        "mcp__peekaboo__app"
    );
    assert_eq!(
        ranker.rank("screenshot window", &peekaboo_docs, 1)[0].name,
        "mcp__peekaboo__image"
    );
    assert_eq!(
        ranker.rank("window", &peekaboo_docs, 1)[0].name,
        "mcp__peekaboo__window"
    );
}

#[test]
fn codemode_source_and_declarations_match_upstream_contracts() {
    let parsed =
        parse_codemode_source("// @options: {\"timeout_ms\": 10}\nconst a = 1;\ntext(a)").unwrap();
    assert_eq!(parsed.code, "\nconst a = 1;\ntext(a)");
    assert_eq!(parsed.options.timeout_ms, Some(10));

    let misplaced =
        parse_codemode_source("// comment\n// @options: {\"timeout_ms\": 1000}\nwhile (true) {}")
            .unwrap_err();
    assert_eq!(
        misplaced.to_string(),
        "The // @options: line must be the first line of the input; found one on line 2"
    );

    assert_eq!(
        schema_to_type(
            &json!({
                "type": "object",
                "properties": {
                    "city": { "type": "string" },
                    "max-lines": { "type": "number" }
                },
                "required": ["city"],
                "additionalProperties": false
            }),
            SchemaToTypeOptions::default()
        ),
        "{ city: string; \"max-lines\"?: number; }"
    );

    let constrained = schema_to_type(
        &json!({
            "type": "object",
            "properties": {
                "timeoutMs": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 30000,
                    "default": 0,
                    "description": "Wait in milliseconds."
                },
                "id": { "type": "string", "pattern": "^[a-z]+$", "maxLength": 8 },
                "tags": { "type": "array", "items": { "type": "string" }, "maxItems": 5, "uniqueItems": true, "deprecated": true },
                "name": { "type": "string" }
            },
            "required": ["id"]
        }),
        SchemaToTypeOptions::default(),
    );
    assert_eq!(
        constrained,
        [
            "{",
            "  // @maxLength 8 @pattern ^[a-z]+$",
            "  id: string;",
            "  name?: string;",
            "  // @maxItems 5 @uniqueItems @deprecated",
            "  tags?: Array<string>;",
            "  // Wait in milliseconds.",
            "  // @integer @minimum 0 @maximum 30000 @default 0",
            "  timeoutMs?: number;",
            "}"
        ]
        .join("\n")
    );

    // Budget fallback: strips tags first, then returns unknown
    let small_schema = json!({
        "type": "object",
        "properties": { "n": { "type": "integer", "minimum": 1 } }
    });
    assert_eq!(
        schema_to_type(
            &small_schema,
            SchemaToTypeOptions {
                max_chars: Some(20)
            }
        ),
        "{ n?: number; }"
    );
    assert_eq!(
        schema_to_type(
            &small_schema,
            SchemaToTypeOptions {
                max_chars: Some(10)
            }
        ),
        "unknown"
    );

    // Recursive $ref resolution stops cleanly
    let ref_schema = json!({
        "type": "object",
        "properties": {
            "item": { "$ref": "#/$defs/Item" }
        },
        "required": ["item"],
        "$defs": {
            "Item": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "parent": { "$ref": "#/$defs/Item" }
                },
                "required": ["id"]
            }
        }
    });
    assert_eq!(
        schema_to_type(&ref_schema, SchemaToTypeOptions::default()),
        "{ item: { id: string; parent?: unknown; }; }"
    );

    let decl = render_declarations(&RenderDeclarationsOptions {
        tools: vec![CodemodeDeclarationItem {
            name: "read-file".to_owned(),
            description: Some("Read a file".to_owned()),
            input_schema: Some(json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            })),
            output_schema: Some(json!({ "type": "string" })),
            signature: None,
        }],
        globals: Vec::new(),
    });
    assert!(decl.contains("read_file(args: { path: string; }): Promise<string>;"));
    let sample = render_tool_sample(
        &CodemodeDeclarationItem {
            name: "echo".to_owned(),
            description: Some("Echo input".to_owned()),
            input_schema: Some(json!({ "type": "string" })),
            output_schema: None,
            signature: None,
        },
        None,
    );
    assert!(sample.contains(&render_tool_signature(
        &CodemodeDeclarationItem {
            name: "echo".to_owned(),
            description: None,
            input_schema: Some(json!({ "type": "string" })),
            output_schema: None,
            signature: None,
        },
        None
    )));

    // Codemode description round-robin budget & deferred filtering
    let plain = stub_agent_tool("read_notes", "Read notes.", json!({}));
    let gh_a = stub_agent_tool("mcp__github__a", "GitHub a.", json!({}));
    let gh_b = stub_agent_tool("mcp__github__b", "GitHub b.", json!({}));
    let docs_s = stub_agent_tool("mcp__docs__search", "Search docs.", json!({}));
    let docs_l = stub_agent_tool("mcp__docs__long", &"Long ".repeat(200), json!({}));
    let all = vec![plain.clone(), gh_a, gh_b, docs_s.clone(), docs_l.clone()];
    let mut namespaces = BTreeMap::new();
    for n in ["mcp__github__a", "mcp__github__b"] {
        namespaces.insert(
            n.to_owned(),
            ToolNamespace {
                name: "mcp__github".to_owned(),
                aliases: Vec::new(),
                description: Some("GitHub server".to_owned()),
                instructions: Some("Long usage guide.".to_owned()),
            },
        );
    }
    for n in ["mcp__docs__search", "mcp__docs__long"] {
        namespaces.insert(
            n.to_owned(),
            ToolNamespace {
                name: "mcp__docs".to_owned(),
                aliases: Vec::new(),
                description: None,
                instructions: None,
            },
        );
    }

    let budgeted = create_codemode_description(
        &all,
        &CodemodeDescriptionOptions {
            namespaces: namespaces.clone(),
            deferred: BTreeSet::new(),
            inline_budget: Some(170),
            docs_path: None,
        },
    );
    assert!(budgeted.contains("### `read_notes`"));
    assert!(budgeted.contains("## mcp__docs (some tools not listed)"));
    assert!(budgeted.contains("### `mcp__docs__search`"));
    assert!(!budgeted.contains("### `mcp__docs__long`"));
    assert!(!budgeted.contains("Long usage guide."));

    let mut deferred = BTreeSet::new();
    deferred.insert("mcp__github__a".to_owned());
    deferred.insert("mcp__github__b".to_owned());
    let without_gh = create_codemode_description(
        &all,
        &CodemodeDescriptionOptions {
            namespaces,
            deferred,
            inline_budget: None,
            docs_path: None,
        },
    );
    assert!(!without_gh.contains("mcp__github"));
}

#[tokio::test]
async fn codemode_quickjs_sandbox_executes_tools_enforces_limits_and_runs_deterministically() {
    let echo = CodemodeTool::new("echo", |args, _cancel| async move { Ok(args) });
    let sandbox = CodemodeSandbox::new(CodemodeSandboxOptions {
        tools: vec![echo],
        globals: Vec::new(),
        timeout_ms: Some(10_000),
        memory_limit_bytes: None,
        max_stack_size: None,
        interrupt_budget: Some(25_000),
    })
    .unwrap();

    // 1. Return value, top-level await, parallel tool calls, output & store
    let png = "iVBORw0KGgo=";
    let res = sandbox
        .execute(
            &format!(
                r#"
                console.log("hello", 1, {{ a: 1 }});
                text({{ json: true }});
                image("data:image/png;base64,{png}");
                store("counter", (load("counter") ?? 0) + 5);
                const [r1, r2] = await Promise.all([tools.echo({{ x: 10 }}), tools.echo({{ y: 20 }})]);
                return {{ sum: r1.x + r2.y, saved: load("counter") }};
                "#
            ),
            CodemodeExecuteOptions {
                store: BTreeMap::from([("counter".to_owned(), json!(10))]),
                ..CodemodeExecuteOptions::default()
            },
        )
        .await;
    assert!(res.ok, "unexpected error: {:?}", res.error);
    assert_eq!(res.value, Some(json!({ "sum": 30, "saved": 15 })));
    assert_eq!(res.calls.len(), 2);
    assert_eq!(
        res.output,
        vec![
            CodemodeOutputItem::Text {
                text: "hello 1 {\"a\":1}".to_owned(),
                console: true,
            },
            CodemodeOutputItem::Text {
                text: "{\"json\":true}".to_owned(),
                console: false,
            },
            CodemodeOutputItem::Image {
                data: png.to_owned(),
                mime_type: "image/png".to_owned(),
            },
        ]
    );
    assert_eq!(
        res.store_writes.unwrap().set.get("counter"),
        Some(&json!(15))
    );

    // 2. exit() ends early with success and keeps store writes
    let exit_res = sandbox
        .execute(
            r#"
            text("before");
            store("k", 99);
            exit();
            text("after");
            "#,
            CodemodeExecuteOptions::default(),
        )
        .await;
    assert!(exit_res.ok);
    assert_eq!(exit_res.output.len(), 1);
    assert_eq!(
        exit_res.store_writes.unwrap().set.get("k"),
        Some(&json!(99))
    );

    // 3. Unknown tool suggests close matches
    let typo_res = sandbox
        .execute("await tools.ech(1)", CodemodeExecuteOptions::default())
        .await;
    assert!(!typo_res.ok);
    assert!(
        typo_res
            .error
            .unwrap()
            .message
            .contains("Did you mean tools.echo?")
    );

    // 4. Stalled promise detection
    let stalled_res = sandbox
        .execute(
            "await new Promise(() => {})",
            CodemodeExecuteOptions::default(),
        )
        .await;
    assert!(!stalled_res.ok);
    assert!(
        stalled_res
            .error
            .unwrap()
            .message
            .contains("waiting on a promise that can never settle")
    );

    // 5. Synchronous & microtask infinite loop interrupt via interrupt_budget
    let loop_res = sandbox
        .execute("while (true) {}", CodemodeExecuteOptions::default())
        .await;
    assert!(!loop_res.ok);
    assert_eq!(loop_res.error.unwrap().kind, CodemodeErrorKind::Sandbox);

    let microtask_loop_res = sandbox
        .execute(
            "while (true) await null;",
            CodemodeExecuteOptions::default(),
        )
        .await;
    assert!(!microtask_loop_res.ok);
    assert_eq!(
        microtask_loop_res.error.unwrap().kind,
        CodemodeErrorKind::Sandbox
    );

    // 6. Deterministic execution mode (`start` + out-of-order `deliver`)
    let started_at = 1_768_471_200_000.0; // 2026-01-15T10:00:00.000Z
    let det_code = r#"
        const first = await tools.echo([Date.now(), performance.now(), Math.random(), new Date().toISOString(), typeof WeakRef]);
        const [a, b] = await Promise.all([tools.echo("one"), tools.echo("two")]);
        return { first, pair: [a, b], endNow: Date.now(), endPerf: performance.now(), rand2: Math.random() };
    "#;

    let run_deterministic = |delay_second: bool| {
        let sb = sandbox.clone();
        async move {
            let run_holder: Arc<Mutex<Option<Arc<pi_runtime::CodemodeRun>>>> =
                Arc::new(Mutex::new(None));
            let holder_cb = run_holder.clone();
            let seq = Arc::new(AtomicUsize::new(0));
            let run = Arc::new(
                sb.start(
                    det_code,
                    CodemodeRunOptions {
                        started_at,
                        seed: 42,
                        tools: vec![CodemodeDeclaredTool {
                            name: "echo".to_owned(),
                            js_name: None,
                            description: None,
                        }],
                        globals: Vec::new(),
                        timeout_ms: Some(10_000),
                        cancel_token: None,
                        store: BTreeMap::new(),
                        on_started: None,
                        on_call: Arc::new(move |call| {
                            let s = seq.fetch_add(1, Ordering::SeqCst) as u64 + 1;
                            let holder = holder_cb.clone();
                            tokio::spawn(async move {
                                if delay_second && s == 2 {
                                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                                }
                                let r = holder.lock().unwrap().clone().unwrap();
                                let _ = r.deliver(
                                    s,
                                    started_at + (s as f64) * 1000.0,
                                    call.id,
                                    CodemodeOutcome::ok(call.args),
                                );
                            });
                        }),
                        on_idle: None,
                    },
                )
                .unwrap(),
            );
            *run_holder.lock().unwrap() = Some(run.clone());
            run.wait().await
        }
    };

    let det_a = run_deterministic(false).await;
    let det_b = run_deterministic(true).await;
    assert!(det_a.ok, "det_a failed: {:?}", det_a.error);
    assert!(det_b.ok, "det_b failed: {:?}", det_b.error);
    assert_eq!(det_a.value, det_b.value);
    let val = det_a.value.unwrap();
    assert_eq!(val["first"][0].as_f64(), Some(started_at));
    assert_eq!(val["first"][1], json!(0));
    assert_eq!(val["first"][3], json!("2026-01-15T10:00:00.000Z"));
    assert_eq!(val["first"][4], json!("undefined"));
    assert_eq!(val["endNow"].as_f64(), Some(started_at + 3000.0));
    assert_eq!(val["endPerf"], json!(3000));
}

#[tokio::test]
async fn mcp_protocol_client_and_builtin_extensions_work_end_to_end_with_session() {
    let root = std::env::temp_dir().join(format!("pi-phase41-mcp-{}", pi_ai::uuid_v7()));
    let agent_dir = root.join("agent");
    let project_dir = root.join("project");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::create_dir_all(project_dir.join(".pi")).unwrap();

    // 1. MCP config loading & project override
    std::fs::write(
        agent_dir.join("mcp.json"),
        r#"{
            "mcpServers": {
                "docs": {
                    "url": "https://example.com/mcp",
                    "exposure": "codemode",
                    "toolExposure": {
                        "admin_*": "hidden",
                        "search_docs": "deferred"
                    }
                }
            }
        }"#,
    )
    .unwrap();
    std::fs::write(
        project_dir.join(".pi").join("mcp.json"),
        r#"{
            "autoEnableCodemode": true,
            "mcpServers": {
                "docs": {
                    "exposure": "deferred"
                }
            }
        }"#,
    )
    .unwrap();

    let cfg = load_mcp_config(&agent_dir, &project_dir, true);
    assert!(cfg.errors.is_empty(), "errors: {:?}", cfg.errors);
    assert_eq!(cfg.auto_enable_codemode, Some(true));
    assert_eq!(cfg.servers.len(), 1);
    assert_eq!(cfg.servers[0].config.exposure(), McpExposure::Deferred);
    assert_eq!(
        get_mcp_tool_exposure(
            cfg.servers[0].config.exposure(),
            cfg.servers[0].config.tool_exposure(),
            "admin_delete"
        ),
        McpExposure::Hidden
    );
    assert_eq!(
        get_mcp_tool_exposure(
            cfg.servers[0].config.exposure(),
            cfg.servers[0].config.tool_exposure(),
            "read_page"
        ),
        McpExposure::Deferred
    );
    assert!(validate_mcp_server_config("bad name!", &json!({"url": "https://a.com"})).is_err());

    // Tool name sanitization & hash truncation
    let mut seen = BTreeSet::new();
    let n1 = create_mcp_tool_name("my-server", "get-item", |s| seen.contains(s));
    seen.insert(n1.clone());
    let n2 = create_mcp_tool_name("my_server", "get_item", |s| seen.contains(s));
    assert_eq!(n1, "mcp__my_server__get_item");
    assert_ne!(n1, n2);
    assert!(n2.len() <= 64);

    // Schema relocation & 20 KiB output truncation
    let relocated = create_mcp_result_schema(Some(&json!({
        "type": "object",
        "properties": { "node": { "$ref": "#/$defs/Node" } },
        "$defs": { "Node": { "type": "string" } }
    })));
    assert_eq!(
        relocated["properties"]["structuredContent"]["properties"]["node"]["$ref"],
        "#/properties/structuredContent/$defs/Node"
    );
    let huge_text = "abcd".repeat(8000);
    let (limited, saved_path) = limit_mcp_content(vec![UserContentBlock::text(huge_text)]);
    assert!(saved_path.is_some());
    if let UserContentBlock::Text(t) = &limited[0] {
        assert!(t.text.contains("Warning: truncated output"));
        assert!(t.text.contains("chars truncated"));
    }

    // 2. Connect McpClient over InMemoryTransport to a mock MCP server
    let (client_transport, server_transport) = InMemoryTransport::pair();
    let mut server_rx = server_transport.start().await.unwrap();
    let server_tx = server_transport.clone();

    tokio::spawn(async move {
        while let Some(msg) = server_rx.recv().await {
            let Some(obj) = msg.as_object() else {
                continue;
            };
            let Some(method) = obj.get("method").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(id) = obj.get("id").cloned() else {
                continue;
            };
            let result = match method {
                "initialize" => json!({
                    "protocolVersion": "2026-07-28",
                    "capabilities": {
                        "tools": {},
                        "resources": {}
                    },
                    "serverInfo": {
                        "name": "docs-server",
                        "version": "1.0.0",
                        "title": "Documentation Server"
                    },
                    "instructions": "Use search_docs to search product guides."
                }),
                "tools/list" => json!({
                    "tools": [{
                        "name": "search_docs",
                        "description": "Search product documentation guides by keyword.",
                        "inputSchema": {
                            "type": "object",
                            "properties": { "query": { "type": "string" } },
                            "required": ["query"]
                        },
                        "outputSchema": {
                            "type": "object",
                            "properties": { "hits": { "type": "number" }, "top": { "type": "string" } },
                            "required": ["hits", "top"]
                        }
                    }]
                }),
                "tools/call" => {
                    let q = obj
                        .get("params")
                        .and_then(|p| p.get("arguments"))
                        .and_then(|a| a.get("query"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    json!({
                        "content": [{ "type": "text", "text": format!("Found guide for {q}") }],
                        "structuredContent": { "hits": 1, "top": format!("guide:{q}") }
                    })
                }
                "resources/list" => json!({
                    "resources": [
                        { "uri": "docs://readme", "name": "README", "mimeType": "text/markdown" },
                        { "uri": "ui://hidden-app", "name": "Hidden UI" }
                    ]
                }),
                "resources/read" => {
                    let uri = obj
                        .get("params")
                        .and_then(|p| p.get("uri"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    json!({
                        "contents": [{
                            "uri": uri,
                            "mimeType": "text/markdown",
                            "text": "# Architecture Overview\nAll systems nominal."
                        }]
                    })
                }
                _ => json!({}),
            };
            let _ = server_tx
                .send(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result,
                }))
                .await;
        }
    });

    let client = Arc::new(McpClient::new(McpClientOptions::default()));
    let init = client.connect(client_transport).await.unwrap();
    assert_eq!(init.server_info.name, "docs-server");
    let mcp_tools = client
        .list_tools(McpRequestOptions::default())
        .await
        .unwrap();
    assert_eq!(mcp_tools.len(), 1);

    // 3. Create AgentSession with built-in tool_search, codemode, and mcp extensions + an orchestrator test tool
    let probe_ext: ExtensionFactory = Arc::new(|| {
        Ok(Box::new(InlineExtension::new(
            "<probe>",
            |pi: &mut pi_session::ExtensionRegistrar| {
                pi.register_tool(ToolDefinition::new(
                    "run_probe",
                    "run_probe",
                    "Probe tool",
                    json!({
                        "type": "object",
                        "properties": { "step": { "type": "string" } },
                        "required": ["step"]
                    }),
                    Arc::new(|_id, args, _cancel, _on_update, ctx| {
                        Box::pin(async move {
                            let step = args.get("step").and_then(|v| v.as_str()).unwrap_or("");
                            match step {
                                "codemode_write" => {
                                    let outcome = ctx
                                        .execute_tool(
                                            CODEMODE_TOOL_NAME,
                                            json!({
                                                "code": r#"
                                                    const found = await searchTools("documentation guides");
                                                    const ns = await describeNamespace("docs");
                                                    const res = await tools.mcp__docs__search_docs({ query: "quickjs" });
                                                    const list = await tools.list_mcp_resources({});
                                                    const page = await tools.read_mcp_resource({ server: "docs", uri: "docs://readme" });
                                                    store("lastTop", res.structuredContent.top);
                                                    return {
                                                        foundTool: found[0]?.name,
                                                        nsInstructions: ns?.instructions,
                                                        top: res.structuredContent.top,
                                                        resourceCount: list.resources.length,
                                                        readmeText: page.contents[0].text
                                                    };
                                                "#
                                            }),
                                            NestedToolCallOptions::default(),
                                        )
                                        .await;
                                    Ok(outcome.result)
                                }
                                "codemode_read" => {
                                    let outcome = ctx
                                        .execute_tool(
                                            CODEMODE_TOOL_NAME,
                                            json!({
                                                "code": "return load('lastTop');"
                                            }),
                                            NestedToolCallOptions::default(),
                                        )
                                        .await;
                                    Ok(outcome.result)
                                }
                                "tool_search" => {
                                    let outcome = ctx
                                        .execute_tool(
                                            TOOL_SEARCH_TOOL_NAME,
                                            json!({ "query": "search_docs" }),
                                            NestedToolCallOptions::default(),
                                        )
                                        .await;
                                    Ok(outcome.result)
                                }
                                _ => Err("unknown step".to_owned()),
                            }
                        })
                    }),
                ));
                Ok(())
            },
        )))
    });

    let created = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(project_dir.clone()),
        agent_dir: Some(agent_dir.clone()),
        extension_factories: vec![
            create_tool_search_extension(),
            create_codemode_extension(CodemodeExtensionOptions::default()),
            create_mcp_extension(McpExtensionOptions {
                preconnected_servers: vec![PreconnectedMcpServer {
                    name: "docs".to_owned(),
                    exposure: McpExposure::Codemode,
                    tool_exposure: BTreeMap::new(),
                    timeout_ms: 10_000,
                    client,
                    tools: mcp_tools,
                    has_resources: true,
                }],
                auto_enable_codemode: Some(true),
                ..McpExtensionOptions::default()
            }),
            probe_ext,
        ],
        session_manager: Some(
            SessionManager::in_memory(&*project_dir.to_string_lossy(), None, None).unwrap(),
        ),
        ..CreateAgentSessionOptions::default()
    })
    .await
    .unwrap();

    let session = created.session;
    let _ = session
        .extension_runner()
        .emit_lifecycle(pi_session::SessionLifecycleEvent::SessionStart {
            reason: "startup".to_owned(),
            previous_session_file: None,
        })
        .await;

    // `mcp` extension with `exposure: codemode` auto-enables `codemode` on session_start
    assert!(
        session
            .get_active_tool_names()
            .contains(&CODEMODE_TOOL_NAME.to_owned())
    );

    // Execute codemode script that searches MCP tools, describes namespace, calls MCP tool & resource tools, and writes to store()
    let runner = session.extension_runner();
    let codemode_tool_def = runner
        .get_tool_definition(CODEMODE_TOOL_NAME)
        .expect("codemode tool registered");
    let ctx_1 = runner.create_tool_context("call-1", None);
    let cm_res_1 = (codemode_tool_def.execute)(
        "call-1".to_owned(),
        json!({
            "code": r#"
                const found = await searchTools("documentation guides");
                const ns = await describeNamespace("docs");
                const res = await tools.mcp__docs__search_docs({ query: "quickjs" });
                const list = await tools.list_mcp_resources({});
                const page = await tools.read_mcp_resource({ server: "docs", uri: "docs://readme" });
                store("lastTop", res.structuredContent.top);
                return {
                    foundTool: found[0]?.name,
                    nsInstructions: ns?.instructions,
                    top: res.structuredContent.top,
                    resourceCount: list.resources.length,
                    readmeText: page.contents[0].text
                };
            "#
        }),
        None,
        None,
        ctx_1,
    )
    .await
    .unwrap();

    let UserContentBlock::Text(t1) = &cm_res_1.content[0] else {
        panic!("expected text output");
    };
    let parsed_out: JsonValue = serde_json::from_str(&t1.text).unwrap();
    assert_eq!(parsed_out["foundTool"], json!("mcp__docs__search_docs"));
    assert_eq!(
        parsed_out["nsInstructions"],
        json!("Use search_docs to search product guides.")
    );
    assert_eq!(parsed_out["top"], json!("guide:quickjs"));
    assert_eq!(parsed_out["resourceCount"], json!(1)); // ui://hidden-app was filtered out!
    assert_eq!(
        parsed_out["readmeText"],
        json!("# Architecture Overview\nAll systems nominal.")
    );

    // Verify `store()` persisted a `codemode-store` entry in the session branch and `load()` reads it in the next call!
    let branch_entries = session.with_session_manager(|sm| sm.get_branch(None));
    assert!(branch_entries.iter().any(|e| {
        matches!(
            e,
            pi_session::SessionEntry::Custom(c) if c.custom_type == CODEMODE_STORE_ENTRY_TYPE
        )
    }));

    let ctx_2 = runner.create_tool_context("call-2", None);
    let cm_res_2 = (codemode_tool_def.execute)(
        "call-2".to_owned(),
        json!({ "code": "return load('lastTop');" }),
        None,
        None,
        ctx_2,
    )
    .await
    .unwrap();
    let UserContentBlock::Text(t2) = &cm_res_2.content[0] else {
        panic!("expected text output");
    };
    assert_eq!(t2.text, "guide:quickjs");

    // Verify `tool_search` loads `mcp__docs__search_docs`, `list_mcp_resources`, `read_mcp_resource` into active tools!
    assert!(
        !session
            .get_active_tool_names()
            .contains(&"mcp__docs__search_docs".to_owned())
    );
    let ts_def = runner
        .get_tool_definition(TOOL_SEARCH_TOOL_NAME)
        .expect("tool_search registered");
    let ctx_3 = runner.create_tool_context("call-3", None);
    let ts_res = (ts_def.execute)(
        "call-3".to_owned(),
        json!({ "query": "search_docs", "limit": 1 }),
        None,
        None,
        ctx_3,
    )
    .await
    .unwrap();
    assert_eq!(ts_res.details["loaded"], json!(["mcp__docs__search_docs"]));
    assert!(
        session
            .get_active_tool_names()
            .contains(&"mcp__docs__search_docs".to_owned())
    );
    let _ = (
        LIST_MCP_RESOURCES_TOOL,
        READ_MCP_RESOURCE_TOOL,
        McpToolAnnotations::default(),
        McpServerConfig::Stdio,
        CallToolResult::default(),
        McpContentBlock::Text {
            text: String::new(),
        },
    );

    let _ = session.shutdown().await;
    let _ = std::fs::remove_dir_all(root);
}

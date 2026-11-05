use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_agent::{
    Agent, AgentContext, AgentEvent, AgentMessage, AgentOptions, AgentTool, AgentToolResult,
    BeforeToolCallResult, CompileOptions, RunToolCallOptions, StreamFn, compile_schema,
    default_agent_model, run_agent_loop, run_tool_call, try_compile_schema,
    validate_tool_arguments,
};
use pi_ai::faux::{faux_assistant_message, faux_text_message, faux_tool_call};
use pi_ai::{
    AssistantMessage, AssistantMessageEvent, JsonObject, JsonValue, StopReason,
    StreamingJsonParser, ToolCall, UserMessage, create_assistant_message_event_stream,
};
use serde_json::{Value, json};

const BIG_KEY_LEN: usize = 65_536;
static BIG_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

struct TrackingAllocator;

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.size() == BIG_KEY_LEN {
            BIG_ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size == BIG_KEY_LEN {
            BIG_ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;

fn make_call(args: JsonValue) -> ToolCall {
    ToolCall {
        id: "c1".to_owned(),
        name: "probe".to_owned(),
        arguments: args.as_object().cloned().unwrap_or_default(),
        thought_signature: None,
        namespace: None,
        arguments_error: None,
    }
}

fn make_tool(schema: JsonValue) -> AgentTool {
    AgentTool::new(
        "probe",
        "Probe",
        "Review probe",
        schema,
        Arc::new(|_, _, _, _| Box::pin(async { Ok(AgentToolResult::text("ok")) })),
    )
}

fn cooperative_pending_stream() -> StreamFn {
    Arc::new(|model, _, opts| {
        Box::pin(async move {
            if let Some(token) = opts.base.cancel_token {
                token.cancelled().await;
            } else {
                std::future::pending::<()>().await;
            }
            let stream = create_assistant_message_event_stream();
            let mut message = AssistantMessage::new_pending(&model);
            message.stop_reason = StopReason::Aborted;
            stream.push(AssistantMessageEvent::Error {
                reason: StopReason::Aborted,
                error: message,
            });
            stream.end(None);
            stream
        })
    })
}

// =============================================================================
// 1. Runtime $ref cycle & depth guard (validation.rs)
// =============================================================================

#[test]
fn self_reference_probe_child() {
    if std::env::var_os("PI_AGENT_REVIEW_REF_CHILD").is_none() {
        return;
    }
    let validator = compile_schema(&json!({"$ref": "#"}), CompileOptions::default());
    assert!(!validator.check(&json!({"x": 1})));
    let errs = validator.errors(&json!({"x": 1}));
    assert!(!errs.is_empty());
    let _ = validator.coerce(&json!({"x": 1}));
}

#[test]
fn non_consuming_recursive_refs_and_depth_limits_fail_safely_without_stack_overflow() {
    // 1. Subprocess verification for pure self-ref `{"$ref": "#"}`
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "self_reference_probe_child", "--nocapture"])
        .env("PI_AGENT_REVIEW_REF_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "recursive ref crashed process: status={}; stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    // 2. Mutual non-consuming cycle A -> B -> A
    let mutual_cycle = json!({
        "$defs": {
            "A": { "$ref": "#/$defs/B" },
            "B": { "$ref": "#/$defs/A" }
        },
        "$ref": "#/$defs/A"
    });
    let mutual_validator = compile_schema(&mutual_cycle, CompileOptions::default());
    assert!(!mutual_validator.check(&json!({"k": "v"})));
    let mutual_errs = mutual_validator.errors(&json!({"k": "v"}));
    assert!(
        mutual_errs
            .iter()
            .any(|e| e.keyword == "$ref" && e.message.contains("circular reference")),
        "expected circular reference diagnostic, got {mutual_errs:?}"
    );

    // 3. Coercion on non-consuming array cycle `{"type": "array", "items": {"$ref": "#"}}`
    let array_cycle = json!({
        "type": "array",
        "items": { "$ref": "#" }
    });
    let array_cycle_validator = compile_schema(&array_cycle, CompileOptions::default());
    let coerced = array_cycle_validator.coerce(&json!("scalar"));
    assert!(coerced.is_array());

    // 4. Depth boundary check
    let deep_schema = json!({
        "$defs": {
            "Node": {
                "type": "object",
                "properties": {
                    "next": { "$ref": "#/$defs/Node" }
                }
            }
        },
        "$ref": "#/$defs/Node"
    });
    let shallow_validator = compile_schema(
        &deep_schema,
        CompileOptions {
            max_depth: 6,
            ..CompileOptions::default()
        },
    );
    assert!(shallow_validator.check(&json!({"next": {"next": {}}})));
    let too_deep = json!({"next": {"next": {"next": {"next": {"next": {"next": {"next": {}}}}}}}});
    assert!(!shallow_validator.check(&too_deep));
    let depth_errs = shallow_validator.errors(&too_deep);
    assert!(
        depth_errs
            .iter()
            .any(|e| e.keyword == "maxDepth" || e.message.contains("maximum validation depth")),
        "expected maxDepth error, got {depth_errs:?}"
    );

    // 5. Thread reuse after unwind: ensure thread-local CHECK_STACK is clean
    let _ = std::panic::catch_unwind(|| {
        let v = compile_schema(&deep_schema, CompileOptions::default());
        assert!(v.check(&json!({"next": {}})));
        panic!("simulated caller panic");
    });
    let reused_validator = compile_schema(&deep_schema, CompileOptions::default());
    assert!(reused_validator.check(&json!({"next": {}})));
}

// =============================================================================
// 2. ECMA-262 regex constraints, resource limits, & schema compile errors
// =============================================================================

#[test]
fn ecma_regex_lookarounds_unicode_and_official_draft2020_12_suite_pass() {
    // 1. Negative lookahead forbidding `..`
    let path_tool = make_tool(json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "pattern": "^(?!.*\\.\\.).*$" }
        },
        "required": ["path"]
    }));
    assert!(validate_tool_arguments(&path_tool, &make_call(json!({"path": "../secret"}))).is_err());
    assert!(validate_tool_arguments(&path_tool, &make_call(json!({"path": "src/lib.rs"}))).is_ok());

    // 2. Positive lookahead + nested alternation lookahead + backreference
    let complex_tool = make_tool(json!({
        "type": "object",
        "properties": {
            "password": { "type": "string", "pattern": "^(?=.*[a-z])(?=.*\\d)[a-z\\d]{6,}$" },
            "token": { "type": "string", "pattern": "^(?:(?!foo|bar)[a-z])+$" },
            "repeated": { "type": "string", "pattern": "^([a-z]{2})-\\1$" }
        },
        "required": ["password", "token", "repeated"]
    }));
    assert!(
        validate_tool_arguments(
            &complex_tool,
            &make_call(json!({
                "password": "abc123",
                "token": "bazqux",
                "repeated": "ab-ab"
            }))
        )
        .is_ok()
    );
    assert!(
        validate_tool_arguments(
            &complex_tool,
            &make_call(json!({
                "password": "abcdef",
                "token": "bazqux",
                "repeated": "ab-ab"
            }))
        )
        .is_err()
    );
    assert!(
        validate_tool_arguments(
            &complex_tool,
            &make_call(json!({
                "password": "abc123",
                "token": "foobaz",
                "repeated": "ab-ab"
            }))
        )
        .is_err()
    );
    assert!(
        validate_tool_arguments(
            &complex_tool,
            &make_call(json!({
                "password": "abc123",
                "token": "bazqux",
                "repeated": "ab-cd"
            }))
        )
        .is_err()
    );

    // 3. Invalid regex pattern is rejected by try_compile_schema / AgentTool::try_new AND never silently dropped by compile_schema
    let invalid_schema = json!({
        "type": "object",
        "properties": {
            "bad": { "type": "string", "pattern": "[unclosed" }
        },
        "required": ["bad"]
    });
    let compile_err = try_compile_schema(&invalid_schema, CompileOptions::default())
        .expect_err("invalid regex must fail try_compile_schema");
    assert_eq!(compile_err.path, "/bad");
    assert_eq!(compile_err.keyword, "pattern");

    assert!(
        AgentTool::try_new(
            "bad_tool",
            "Bad",
            "Bad",
            invalid_schema.clone(),
            Arc::new(|_, _, _, _| Box::pin(async { Ok(AgentToolResult::text("ok")) }))
        )
        .is_err()
    );

    let fallback_tool = make_tool(invalid_schema);
    let fallback_err =
        validate_tool_arguments(&fallback_tool, &make_call(json!({"bad": "anything"}))).expect_err(
            "non-Result compile_schema must still reject when schema has invalid pattern",
        );
    assert!(
        fallback_err.contains("invalid regex pattern"),
        "unexpected error: {fallback_err}"
    );

    // 4. Native execution budget bounds nested quantifiers and backreferences
    let evil_schema = json!({
        "type": "string",
        "pattern": "^((a+)b?)+\\1c$"
    });
    let bounded_validator = compile_schema(
        &evil_schema,
        CompileOptions {
            regex_backtrack_limit: 200,
            ..CompileOptions::default()
        },
    );
    let evil_input = Value::String("a".repeat(30));
    assert!(!bounded_validator.check(&evil_input));
    let evil_errs = bounded_validator.errors(&evil_input);
    assert!(
        evil_errs
            .iter()
            .any(|e| e.keyword == "pattern" && e.message.contains("limit exceeded")),
        "expected backtrack limit error, got {evil_errs:?}"
    );

    for nested_pat in ["^(a+)+$", "^(?=(a+)+b)a+$"] {
        let nested_validator = try_compile_schema(
            &json!({ "type": "string", "pattern": nested_pat }),
            CompileOptions {
                regex_backtrack_limit: 1,
                ..CompileOptions::default()
            },
        )
        .expect("nested repetition pattern must compile");
        let probe_text = format!("{}!", "a".repeat(32));
        let started = std::time::Instant::now();
        assert!(!nested_validator.check(&json!(probe_text)));
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "nested repetition {nested_pat} took {:?} on 33 bytes",
            started.elapsed()
        );
    }

    // 5. Self-contained ECMA-262 conformance cases (always executed in CI, no external disk path dependency)
    let inline_cases: Vec<(Value, Vec<(Value, bool)>)> = vec![
        // ECMA-262 dot excludes \n, \r, \u2028 (LINE SEPARATOR), \u2029 (PARAGRAPH SEPARATOR)
        (
            json!({ "type": "string", "pattern": "^a.b$" }),
            vec![
                (json!("axb"), true),
                (json!("a\nb"), false),
                (json!("a\rb"), false),
                (json!("a\u{2028}b"), false),
                (json!("a\u{2029}b"), false),
            ],
        ),
        // ECMA-262 $ does not match before trailing newline
        (
            json!({ "type": "string", "pattern": "^abc$" }),
            vec![(json!("abc"), true), (json!("abc\n"), false)],
        ),
        // ECMA-262 word boundaries \b and \B use ASCII word characters [A-Za-z0-9_]
        (
            json!({ "type": "string", "pattern": "\\Badmin\\B" }),
            vec![
                (json!("xadminx"), true),
                (json!("éadminé"), false),
                (json!(" admin "), false),
            ],
        ),
        (
            json!({ "type": "string", "pattern": "\\badmin\\b" }),
            vec![(json!("éadminé"), true), (json!("xadminx"), false)],
        ),
        // ECMA-262 \s and \S (both bare and inside character classes [\s] / [\S]) include \uFEFF BOM and \u2029
        (
            json!({ "type": "string", "pattern": "^\\s$" }),
            vec![
                (json!(" "), true),
                (json!("\t"), true),
                (json!("\u{00a0}"), true),
                (json!("\u{feff}"), true),
                (json!("\u{2029}"), true),
                (json!("\u{2003}"), true),
                (json!("\u{0001}"), false),
            ],
        ),
        (
            json!({ "type": "string", "pattern": "^[\\S]$" }),
            vec![
                (json!("a"), true),
                (json!(" "), false),
                (json!("\u{feff}"), false),
                (json!("\u{2029}"), false),
                (json!("\u{0001}"), true),
            ],
        ),
        // ECMA-262 character class [a&&b] in u-mode treats & as literal, not Rust set intersection
        (
            json!({ "type": "string", "pattern": "^[a&&b]$" }),
            vec![
                (json!("a"), true),
                (json!("b"), true),
                (json!("&"), true),
                (json!("c"), false),
            ],
        ),
        // ECMA-262 \d / \D and \w / \W are ASCII-only
        (
            json!({ "type": "string", "pattern": "^\\d+$" }),
            vec![
                (json!("42"), true),
                (json!("߀"), false),
                (json!("৪২"), false),
            ],
        ),
        (
            json!({ "type": "string", "pattern": "^\\w+$" }),
            vec![(json!("ecole_1"), true), (json!("école"), false)],
        ),
        // ECMA-262 \cC / \cc control escapes
        (
            json!({ "type": "string", "pattern": "^\\cC\\cc$" }),
            vec![
                (json!("\u{0003}\u{0003}"), true),
                (json!("\\cC\\cc"), false),
            ],
        ),
        // Unicode property escapes \p{Letter} and astral plane non-BMP characters in pattern & patternProperties
        (
            json!({
                "type": "object",
                "patternProperties": {
                    "^\\p{Letter}+$": { "type": "string", "pattern": "^🐲*$" }
                },
                "additionalProperties": false
            }),
            vec![
                (json!({ "π": "🐲🐲" }), true),
                (json!({ "Hello": "" }), true),
                (json!({ "π": "🐉" }), false),
                (json!({ "123": "🐲" }), false),
            ],
        ),
    ];

    for (schema, cases) in inline_cases {
        let validator = compile_schema(
            &schema,
            CompileOptions {
                coerce: false,
                ..CompileOptions::default()
            },
        );
        for (input, expected) in cases {
            let actual = validator.check(&input);
            assert_eq!(
                actual, expected,
                "ECMA-262 inline conformance mismatch for schema={schema} input={input}"
            );
        }
    }
}

// =============================================================================
// 3. before_tool_call explicit parameter mutation & replacement
// =============================================================================

#[tokio::test]
async fn before_hook_mutation_and_explicit_replacement_reach_execute_and_after_without_touching_transcript()
 {
    let seen = Arc::new(Mutex::new(Vec::<JsonValue>::new()));
    let seen_exec = seen.clone();
    let mut t = make_tool(json!({
        "type": "object",
        "properties": { "path": { "type": "string" } },
        "required": ["path"],
        "additionalProperties": false
    }));
    t.execute = Arc::new(move |_, args, _, _| {
        seen_exec.lock().unwrap().push(args);
        Box::pin(async { Ok(AgentToolResult::text("ok")) })
    });

    // Case 1: In-place mutation via `&mut BeforeToolCallContext` (also adding an internal field not in schema, proving no re-validation)
    let seen_after = seen.clone();
    let call = make_call(json!({"path": "/unsafe"}));
    let outcome = run_tool_call(
        call.clone(),
        RunToolCallOptions {
            tools: vec![t.clone()],
            assistant_message: faux_text_message("tool request"),
            context: AgentContext::default(),
            cancel_token: None,
            on_update: None,
            before_tool_call: Some(Arc::new(|ctx, _| {
                Box::pin(async move {
                    ctx.args["path"] = json!("/sandbox/safe");
                    ctx.args["injectedInternal"] = json!(true);
                    Ok(None)
                })
            })),
            after_tool_call: Some(Arc::new(move |ctx, _| {
                seen_after.lock().unwrap().push(ctx.args);
                Box::pin(async { Ok(None) })
            })),
        },
    )
    .await;

    assert!(!outcome.is_error);
    assert_eq!(call.arguments["path"], json!("/unsafe"));
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            json!({"path": "/sandbox/safe", "injectedInternal": true}),
            json!({"path": "/sandbox/safe", "injectedInternal": true}),
        ]
    );

    // Case 2: Explicit replacement via `BeforeToolCallResult.args` takes precedence over `ctx.args`
    seen.lock().unwrap().clear();
    let seen_after_2 = seen.clone();
    let outcome_2 = run_tool_call(
        call.clone(),
        RunToolCallOptions {
            tools: vec![t],
            assistant_message: faux_text_message("tool request 2"),
            context: AgentContext::default(),
            cancel_token: None,
            on_update: None,
            before_tool_call: Some(Arc::new(|ctx, _| {
                Box::pin(async move {
                    ctx.args["path"] = json!("/mutated-first");
                    Ok(Some(BeforeToolCallResult {
                        args: Some(json!({"path": "/explicit-override"})),
                        ..Default::default()
                    }))
                })
            })),
            after_tool_call: Some(Arc::new(move |ctx, _| {
                seen_after_2.lock().unwrap().push(ctx.args);
                Box::pin(async { Ok(None) })
            })),
        },
    )
    .await;

    assert!(!outcome_2.is_error);
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            json!({"path": "/explicit-override"}),
            json!({"path": "/explicit-override"}),
        ]
    );
}

// =============================================================================
// 4. Cancel-safe run lifecycle, panic recovery, & ToolUpdateGate cleanup
// =============================================================================

#[tokio::test]
async fn dropping_prompt_and_continue_futures_settle_cleanly_and_permit_reuse() {
    let agent = Agent::new(AgentOptions::new(cooperative_pending_stream()));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), agent.prompt("hello"))
            .await
            .is_err()
    );
    agent.abort();
    let idle_completed = tokio::time::timeout(Duration::from_millis(250), agent.wait_for_idle())
        .await
        .is_ok();
    let streaming = agent.state().is_streaming;
    let active = agent.cancel_token().is_some();
    let reset = agent.reset();
    let retry = tokio::time::timeout(Duration::from_millis(20), agent.prompt("retry")).await;
    agent.abort();
    let _ = tokio::time::timeout(Duration::from_millis(250), agent.wait_for_idle()).await;

    assert!(
        !streaming && !active && idle_completed && reset.is_ok() && retry.is_err(),
        "dropping a prompt future must settle within a finite timeout and permit reuse"
    );

    // Also test dropping `continue_run()` future
    agent.set_messages(vec![AgentMessage::User(UserMessage::new(
        "continue from user",
    ))]);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), agent.continue_run())
            .await
            .is_err()
    );
    let cont_idle = tokio::time::timeout(Duration::from_millis(250), agent.wait_for_idle())
        .await
        .is_ok();
    assert!(cont_idle);
    assert!(!agent.state().is_streaming);
    assert!(agent.reset().is_ok());
}

#[tokio::test]
async fn panicking_stream_and_panicking_tool_with_active_updates_settle_cleanly() {
    // 1. Panicking stream_fn settles lifecycle and records error
    let stream_fn: StreamFn =
        Arc::new(|_, _, _| Box::pin(async { panic!("review injected provider panic") }));
    let agent = Agent::new(AgentOptions::new(stream_fn));
    let runner = agent.clone();
    let handle = tokio::spawn(async move { runner.prompt("hello").await });
    let _ = handle.await;
    assert!(
        !agent.state().is_streaming,
        "unwind bypassed finish_run and left active_run installed"
    );
    assert!(
        agent
            .state()
            .error_message
            .as_deref()
            .is_some_and(|m| m.contains("review injected provider panic"))
    );

    // 2. Tool pushes updates while sink is awaiting, then tool panics:
    // Gate must close, drain accepted updates in order before ToolExecutionEnd, and reclaim worker!
    let delivered_updates = Arc::new(Mutex::new(Vec::<String>::new()));
    let panicking_tool = AgentTool::new(
        "boom_updater",
        "Boom",
        "Pushes updates then panics",
        json!({"type": "object"}),
        Arc::new(|_id, _args, _cancel, on_update| {
            Box::pin(async move {
                let cb = on_update.expect("on_update must be present");
                cb(AgentToolResult::text("u1"));
                cb(AgentToolResult::text("u2"));
                tokio::time::sleep(Duration::from_millis(5)).await;
                panic!("tool exploded mid-stream");
            })
        }),
    );

    let mut tc_args = JsonObject::new();
    tc_args.insert("x".to_owned(), json!(1));
    let assistant_with_boom = faux_assistant_message(
        vec![faux_tool_call(
            "boom_updater",
            tc_args,
            Some("call_boom".to_owned()),
        )],
        Some(StopReason::ToolUse),
    );
    let final_reply = faux_text_message("recovered from tool panic");
    let queue = Arc::new(Mutex::new(std::collections::VecDeque::from(vec![
        assistant_with_boom,
        final_reply,
    ])));
    let scripted_stream: StreamFn = Arc::new(move |model, _, _| {
        let queue = queue.clone();
        Box::pin(async move {
            let stream = create_assistant_message_event_stream();
            let mut msg = queue.lock().unwrap().pop_front().unwrap();
            msg.model = model.id.clone();
            stream.push(AssistantMessageEvent::Start {
                partial: AssistantMessage::new_pending(&model),
            });
            stream.push(AssistantMessageEvent::Done {
                reason: msg.stop_reason,
                message: msg,
            });
            stream.end(None);
            stream
        })
    });

    let delivered_clone = delivered_updates.clone();
    let saw_end = Arc::new(AtomicBool::new(false));
    let saw_end_clone = saw_end.clone();
    let emit = Arc::new(move |ev: AgentEvent| {
        let delivered_clone = delivered_clone.clone();
        let saw_end_clone = saw_end_clone.clone();
        Box::pin(async move {
            match ev {
                AgentEvent::ToolExecutionUpdate { partial_result, .. } => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    assert!(
                        !saw_end_clone.load(Ordering::SeqCst),
                        "ToolExecutionUpdate arrived after ToolExecutionEnd"
                    );
                    if let Some(t) = partial_result.first_text() {
                        delivered_clone.lock().unwrap().push(t.to_owned());
                    }
                }
                AgentEvent::ToolExecutionEnd {
                    is_error, result, ..
                } => {
                    assert!(is_error);
                    assert!(
                        result
                            .first_text()
                            .is_some_and(|t| t.contains("tool exploded mid-stream"))
                    );
                    saw_end_clone.store(true, Ordering::SeqCst);
                }
                _ => {}
            }
        }) as _
    });

    let new_msgs = run_agent_loop(
        vec![AgentMessage::User(UserMessage::new("run boom"))],
        AgentContext {
            messages: Vec::new(),
            tools: vec![panicking_tool],
        },
        pi_agent::AgentLoopConfig::new(default_agent_model()),
        emit,
        None,
        scripted_stream,
    )
    .await;

    assert!(saw_end.load(Ordering::SeqCst));
    assert_eq!(
        *delivered_updates.lock().unwrap(),
        vec!["u1".to_owned(), "u2".to_owned()]
    );
    assert_eq!(new_msgs.last().unwrap().role(), "assistant");
}

#[tokio::test]
async fn agent_end_listener_settlement_barrier_holds_during_abort_and_isolates_runs() {
    let agent = Agent::new(AgentOptions::new(cooperative_pending_stream()));
    let listener_settled = Arc::new(AtomicBool::new(false));
    let settled_flag = listener_settled.clone();

    let _unsub = agent.subscribe(Arc::new(move |ev, _cancel| {
        let settled_flag = settled_flag.clone();
        Box::pin(async move {
            if matches!(ev, AgentEvent::AgentEnd { .. }) {
                tokio::time::sleep(Duration::from_millis(60)).await;
                settled_flag.store(true, Ordering::SeqCst);
            }
        })
    }));

    // Drop prompt after 15ms; AgentEnd listener takes 60ms to settle
    assert!(
        tokio::time::timeout(Duration::from_millis(15), agent.prompt("first"))
            .await
            .is_err()
    );
    // Immediately after timeout, AgentEnd listener has NOT finished yet, so agent must still be busy!
    assert!(!listener_settled.load(Ordering::SeqCst));
    assert!(agent.state().is_streaming);

    // Wait for full settlement
    agent.wait_for_idle().await;
    assert!(listener_settled.load(Ordering::SeqCst));
    assert!(!agent.state().is_streaming);
}

#[tokio::test]
async fn cancellation_enforces_single_shared_deadline_and_preserves_accumulated_partial_content() {
    // 1. Forced cancellation after Start + TextDelta + ThinkingDelta + ToolCallStart preserves all partial blocks
    let stream_fn_partial: StreamFn = Arc::new(|model, _, _| {
        Box::pin(async move {
            let stream = create_assistant_message_event_stream();
            let pending = AssistantMessage::new_pending(&model);
            stream.push(AssistantMessageEvent::Start {
                partial: pending.clone(),
            });
            let mut partial = pending;
            partial
                .content
                .push(pi_ai::ContentBlock::Thinking(pi_ai::ThinkingContent {
                    thinking: "partial reasoning".to_owned(),
                    thinking_signature: Some("sig_partial".to_owned()),
                    redacted: None,
                }));
            partial
                .content
                .push(pi_ai::ContentBlock::Text(pi_ai::TextContent::new(
                    "already streamed text",
                )));
            let mut tc_args = JsonObject::new();
            tc_args.insert("cmd".to_owned(), json!("ls"));
            partial
                .content
                .push(pi_ai::ContentBlock::ToolCall(ToolCall {
                    id: "tc_part".to_owned(),
                    name: "bash".to_owned(),
                    arguments: tc_args,
                    thought_signature: Some("tc_sig".to_owned()),
                    namespace: None,
                    arguments_error: None,
                }));
            partial.usage.output = 19;
            stream.push(AssistantMessageEvent::TextDelta {
                content_index: 1,
                delta: "already streamed text".to_owned(),
                partial,
            });
            // Deliberately never emit Done/Error
            stream
        })
    });

    let agent_1 = Agent::new(AgentOptions::new(stream_fn_partial));
    let msg_starts_1 = Arc::new(AtomicUsize::new(0));
    let ms1 = msg_starts_1.clone();
    let _unsub_1 = agent_1.subscribe(Arc::new(move |event, token| {
        let ms1 = ms1.clone();
        Box::pin(async move {
            if let AgentEvent::MessageStart { message } = &event
                && message.role() == "assistant"
            {
                ms1.fetch_add(1, Ordering::SeqCst);
            }
            if matches!(event, AgentEvent::MessageUpdate { .. }) {
                token.cancel();
            }
        })
    }));

    tokio::time::timeout(Duration::from_secs(1), agent_1.prompt("go"))
        .await
        .expect("must settle after cancel")
        .unwrap();

    let state_1 = agent_1.state();
    let msg_1 = state_1.messages.last().unwrap().as_assistant().unwrap();
    assert_eq!(msg_1.stop_reason, StopReason::Aborted);
    assert_eq!(msg_1.text(), "already streamed text");
    assert_eq!(msg_1.thinking(), "partial reasoning");
    assert_eq!(msg_1.tool_calls().len(), 1);
    assert_eq!(msg_1.tool_calls()[0].id, "tc_part");
    assert_eq!(msg_1.usage.output, 19);
    assert_eq!(
        msg_starts_1.load(Ordering::SeqCst),
        1,
        "must not emit a second assistant MessageStart when Start already occurred"
    );

    // 2. Non-cooperative provider emitting deltas every 2ms must be cut off by the single 50ms grace deadline
    let stop_flag = Arc::new(AtomicBool::new(false));
    let provider_stop = stop_flag.clone();
    let stream_fn_spam: StreamFn = Arc::new(move |model, _, _| {
        let stop = provider_stop.clone();
        Box::pin(async move {
            let stream = create_assistant_message_event_stream();
            let writer = stream.clone();
            tokio::spawn(async move {
                let mut partial = AssistantMessage::new_pending(&model);
                writer.push(AssistantMessageEvent::Start {
                    partial: partial.clone(),
                });
                partial
                    .content
                    .push(pi_ai::ContentBlock::Text(pi_ai::TextContent::new("x")));
                while !stop.load(Ordering::SeqCst) {
                    writer.push(AssistantMessageEvent::TextDelta {
                        content_index: 0,
                        delta: "x".to_owned(),
                        partial: partial.clone(),
                    });
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            });
            stream
        })
    });

    let agent_2 = Agent::new(AgentOptions::new(stream_fn_spam));
    let _unsub_2 = agent_2.subscribe(Arc::new(|event, token| {
        Box::pin(async move {
            if matches!(event, AgentEvent::MessageUpdate { .. }) {
                token.cancel();
            }
        })
    }));

    let completion = tokio::time::timeout(Duration::from_millis(300), agent_2.prompt("go")).await;
    stop_flag.store(true, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(1), agent_2.wait_for_idle())
        .await
        .expect("agent must be idle");
    assert!(
        completion.is_ok(),
        "cancelled run must enforce one overall 50ms grace deadline instead of resetting per delta"
    );
}

// =============================================================================
// 5. AgentTool schema & compiled validator cache immutability
// =============================================================================

#[test]
fn replacing_parameters_creates_independent_validator_without_affecting_original_or_clones() {
    let original = make_tool(json!({
        "type": "object",
        "properties": { "n": { "type": "integer" } },
        "required": ["n"]
    }));
    // Warm up original's validator cache
    assert!(validate_tool_arguments(&original, &make_call(json!({"n": 1}))).is_ok());

    let mut changed = original.clone();
    changed.set_parameters(json!({
        "type": "object",
        "properties": { "n": { "type": "integer", "minimum": 100 } },
        "required": ["n"]
    }));

    // Changed tool must enforce minimum: 100
    let result = validate_tool_arguments(&changed, &make_call(json!({"n": 1})));
    assert!(
        result.is_err(),
        "new declaration's minimum=100 must be enforced, got {result:?}"
    );
    assert!(validate_tool_arguments(&changed, &make_call(json!({"n": 100}))).is_ok());

    // Original tool must still accept n = 1
    assert!(validate_tool_arguments(&original, &make_call(json!({"n": 1}))).is_ok());

    // Concurrent multi-threaded validation across original and forked tool
    let mut handles = Vec::new();
    for _ in 0..4 {
        let orig_t = original.clone();
        let chg_t = changed.clone();
        handles.push(std::thread::spawn(move || {
            for _ in 0..50 {
                assert!(validate_tool_arguments(&orig_t, &make_call(json!({"n": 5}))).is_ok());
                assert!(validate_tool_arguments(&chg_t, &make_call(json!({"n": 5}))).is_err());
                assert!(validate_tool_arguments(&chg_t, &make_call(json!({"n": 150}))).is_ok());
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
}

// =============================================================================
// 6. StreamingJsonParser current_key & root closure budget accounting
// =============================================================================

#[test]
fn snapshot_budget_charges_open_ancestor_current_keys_and_moves_closed_root_zero_copy() {
    let mut parser = StreamingJsonParser::new();
    let prefix = format!("{{\"{}\":[", "k".repeat(BIG_KEY_LEN));
    parser.append(&prefix);

    let mut copies = 0usize;
    let before = BIG_ALLOCATIONS.load(Ordering::SeqCst);
    let _ = parser.snapshot_ref();
    copies += BIG_ALLOCATIONS.load(Ordering::SeqCst) - before;

    let appends = 1_000usize;
    for _ in 0..appends {
        parser.append("0,");
        let before = BIG_ALLOCATIONS.load(Ordering::SeqCst);
        let _ = parser.snapshot_ref();
        copies += BIG_ALLOCATIONS.load(Ordering::SeqCst) - before;
    }

    let n = prefix.len() + 2 * appends;
    let k = appends + 1;
    let budget = 8 * n + 256 * k;
    let actual_key_bytes = copies * BIG_KEY_LEN;

    assert!(
        parser.total_snapshot_bytes_copied() <= budget,
        "reported copy cost {} exceeded budget {}",
        parser.total_snapshot_bytes_copied(),
        budget
    );
    assert!(
        actual_key_bytes <= budget,
        "actual key copy bytes {} ({} copies) exceeded budget {}",
        actual_key_bytes,
        copies,
        budget
    );

    // Close the array and root object: final snapshot_ref() must move root into cache with 0 BIG_KEY_LEN allocations!
    parser.append("0]}");
    let before_close = BIG_ALLOCATIONS.load(Ordering::SeqCst);
    let (refreshed, final_snap) = parser.snapshot_ref();
    let close_allocs = BIG_ALLOCATIONS.load(Ordering::SeqCst) - before_close;
    assert!(refreshed);
    assert!(final_snap.is_some());
    assert_eq!(
        close_allocs, 0,
        "closing root container must move finished Value into cached_snapshot without cloning 64KB key"
    );
}

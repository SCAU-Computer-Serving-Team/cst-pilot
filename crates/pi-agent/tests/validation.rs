use std::sync::Arc;

use pi_agent::{
    AgentTool, AgentToolResult, CompileOptions, UnresolvedRefs, compile_schema,
    format_validation_path, validate_tool_arguments, validate_tool_call,
};
use pi_ai::{JsonObject, ToolCall};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

fn make_tool(name: &str, parameters: Value) -> AgentTool {
    AgentTool::new(
        name,
        name,
        format!("Test tool {name}"),
        parameters,
        Arc::new(|_id, _args, _cancel, _on_update| {
            Box::pin(async move { Ok(AgentToolResult::text("ok")) })
        }),
    )
}

fn make_call(name: &str, args: Value) -> ToolCall {
    let map: JsonObject = args.as_object().cloned().unwrap_or_default();
    ToolCall {
        id: "call_1".to_owned(),
        name: name.to_owned(),
        arguments: map,
        thought_signature: None,
        namespace: None,
        arguments_error: None,
    }
}

#[test]
fn three_stage_coercion_handles_optional_nulls_scalars_and_single_value_arrays() {
    let schema = json!({
        "type": "object",
        "properties": {
            "count": { "type": "integer" },
            "ratio": { "type": "number" },
            "enabled": { "type": "boolean" },
            "label": { "type": "string" },
            "tags": {
                "type": "array",
                "items": { "type": "string" }
            },
            "optionalNote": { "type": "string" },
            "nullableNote": { "type": ["integer", "null"] }
        },
        "required": ["count", "ratio", "enabled", "label", "tags"],
        "additionalProperties": false
    });

    let tool = make_tool("coerce_test", schema);
    let call = make_call(
        "coerce_test",
        json!({
            "count": "3.9",
            "ratio": "42.5",
            "enabled": "true",
            "label": 123,
            "tags": "single-tag",
            "optionalNote": null,
            "nullableNote": "17"
        }),
    );

    let validated = validate_tool_arguments(&tool, &call).expect("coercion should succeed");
    assert_eq!(
        validated,
        json!({
            "count": 3,
            "ratio": 42.5,
            "enabled": true,
            "label": "123",
            "tags": ["single-tag"],
            "nullableNote": 17
        })
    );
    // Original tool_call.arguments in transcript must remain untouched
    assert_eq!(call.arguments.get("count"), Some(&json!("3.9")));
    assert!(call.arguments.contains_key("optionalNote"));
}

#[test]
fn tuple_arrays_do_not_wrap_single_values_and_recursive_refs_terminate() {
    let tuple_schema = json!({
        "type": "object",
        "properties": {
            "pair": {
                "type": "array",
                "prefixItems": [
                    { "type": "integer" },
                    { "type": "boolean" }
                ]
            }
        },
        "required": ["pair"]
    });
    let tuple_validator = compile_schema(&tuple_schema, CompileOptions::default());
    // A non-array passed to a tuple schema must NOT be wrapped into a 1-element array
    let err = tuple_validator
        .parse(&json!({ "pair": "42" }))
        .expect_err("tuple should reject scalar");
    assert_eq!(err[0].path, "/pair");
    assert_eq!(err[0].keyword, "type");

    // Valid tuple with stringified elements coerces per position
    let ok = tuple_validator
        .parse(&json!({ "pair": ["7", "false"] }))
        .expect("tuple elements should coerce");
    assert_eq!(ok, json!({ "pair": [7, false] }));

    // Recursive $ref schema terminates on invalid input without infinite recursion
    let recursive_schema = json!({
        "$defs": {
            "Node": {
                "type": "object",
                "properties": {
                    "value": { "type": "integer" },
                    "next": { "$ref": "#/$defs/Node" }
                },
                "required": ["value"]
            }
        },
        "$ref": "#/$defs/Node"
    });
    let rec_validator = compile_schema(&recursive_schema, CompileOptions::default());
    let rec_ok = rec_validator
        .parse(&json!({ "value": "10", "next": { "value": "20" } }))
        .expect("recursive coercion should succeed");
    assert_eq!(rec_ok, json!({ "value": 10, "next": { "value": 20 } }));

    let rec_err = rec_validator
        .parse(&json!({ "value": "10", "next": { "value": "not-a-number" } }))
        .expect_err("invalid recursive leaf should fail");
    assert_eq!(rec_err[0].path, "/next/value");
}

#[test]
fn tagged_union_and_heuristic_union_report_targeted_branch_errors() {
    let schema = json!({
        "type": "object",
        "properties": {
            "action": {
                "anyOf": [
                    {
                        "type": "object",
                        "properties": {
                            "kind": { "const": "read" },
                            "path": { "type": "string", "minLength": 2 }
                        },
                        "required": ["kind", "path"],
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "properties": {
                            "kind": { "const": "write" },
                            "path": { "type": "string" },
                            "lines": { "type": "integer", "minimum": 1 }
                        },
                        "required": ["kind", "path", "lines"],
                        "additionalProperties": false
                    }
                ]
            }
        },
        "required": ["action"]
    });

    let tool = make_tool("union_tool", schema);

    // 1. Matching tag "write", invalid "lines" -> blames only the "write" branch at action.lines
    let call_write = make_call(
        "union_tool",
        json!({
            "action": {
                "kind": "write",
                "path": "a.txt",
                "lines": 0
            }
        }),
    );
    let err_msg = validate_tool_arguments(&tool, &call_write).unwrap_err();
    assert!(
        err_msg.contains("action.lines: expected >= 1"),
        "unexpected error: {err_msg}"
    );

    // 2. Unknown tag value -> blames the tag field itself with expected one of ["read","write"]
    let call_bad_tag = make_call(
        "union_tool",
        json!({
            "action": {
                "kind": "delete",
                "path": "a.txt"
            }
        }),
    );
    let err_tag = validate_tool_arguments(&tool, &call_bad_tag).unwrap_err();
    assert!(
        err_tag.contains("action.kind: expected one of [\"read\",\"write\"]"),
        "unexpected error: {err_tag}"
    );
}

#[test]
fn unresolved_refs_reject_only_when_touched_and_pointer_formatting_works() {
    let schema = json!({
        "type": "object",
        "properties": {
            "safe": { "type": "string" },
            "dangling": { "$ref": "#/$defs/MissingDef" }
        },
        "required": ["safe"]
    });

    let validator = compile_schema(
        &schema,
        CompileOptions {
            coerce: true,
            unresolved_refs: UnresolvedRefs::Reject,
            ..CompileOptions::default()
        },
    );

    // Omitting the dangling property succeeds!
    let ok = validator
        .parse(&json!({ "safe": "hello" }))
        .expect("omitting dangling $ref property should succeed");
    assert_eq!(ok, json!({ "safe": "hello" }));

    // Providing a value at `dangling` fails with unresolved reference error
    let errs = validator
        .parse(&json!({ "safe": "hello", "dangling": 123 }))
        .expect_err("touching dangling $ref should fail");
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].path, "/dangling");
    assert_eq!(errs[0].keyword, "$ref");
    assert_eq!(errs[0].message, "unresolved reference #/$defs/MissingDef");

    // JSON Pointer formatting unescapes ~1 and ~0
    assert_eq!(format_validation_path(""), "root");
    assert_eq!(format_validation_path("/edits/0/a~1b~0c"), "edits.0.a/b~c");
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
struct TypedEditParams {
    path: String,
    line: u32,
    dry_run: Option<bool>,
}

#[tokio::test]
async fn typed_agent_tool_coerces_before_serde_deserialization() {
    let tool = AgentTool::from_typed(
        "typed_edit",
        "Typed Edit",
        "Edits a file",
        |_id, params: TypedEditParams, _cancel, _on_update| async move {
            Ok(AgentToolResult::text(format!(
                "{}:{}:{}",
                params.path,
                params.line,
                params.dry_run.unwrap_or(false)
            )))
        },
    );

    let tools = vec![tool.clone()];
    let call = make_call(
        "typed_edit",
        json!({
            "path": "src/main.rs",
            "line": "42",
            "dry_run": null
        }),
    );

    let coerced = validate_tool_call(&tools, &call).expect("typed tool validation should pass");
    assert_eq!(
        coerced,
        json!({
            "path": "src/main.rs",
            "line": 42
        })
    );

    let res = (tool.execute)("call_1".to_owned(), coerced, None, None)
        .await
        .expect("typed tool execution should succeed");
    assert_eq!(res.first_text(), Some("src/main.rs:42:false"));
}

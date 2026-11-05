use pi_agent::{CompileOptions, try_compile_schema};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct Group {
    format: String,
    tests: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    data: String,
    expected: bool,
}

#[test]
fn date_time_and_email_formats_match_upstream() {
    // Expected results from pi-schema@1.5.0 with coerce:false and its standard formats.
    let groups: Vec<Group> =
        serde_json::from_str(include_str!("fixtures/schema_formats.json")).unwrap();
    for group in groups {
        let validator = try_compile_schema(
            &json!({"type": "string", "format": group.format}),
            CompileOptions::default(),
        )
        .unwrap();
        for case in group.tests {
            let value = json!(case.data);
            assert_eq!(
                validator.check(&value),
                case.expected,
                "format {:?}, input {:?}",
                group.format,
                case.data,
            );
            assert_eq!(
                validator.errors(&value).is_empty(),
                case.expected,
                "error explanation disagrees: format {:?}, input {:?}",
                group.format,
                case.data,
            );
        }
    }
}

#[test]
fn custom_formats_are_compile_local_and_used_by_checks_coercion_and_errors() {
    let schema = json!({
        "type": "object",
        "properties": {"name": {"type": "string", "format": "short-name"}}
    });
    let validator = try_compile_schema(
        &schema,
        CompileOptions::default().with_format("short-name", |s| s.len() <= 3),
    )
    .unwrap();
    assert!(validator.check(&json!({"name": "abc"})));
    let invalid = json!({"name": "abcd"});
    assert!(!validator.check(&invalid));
    let errors = validator.errors(&invalid);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].keyword, "format");
    assert_eq!(errors[0].path, "/name");
    assert!(validator.errors(&json!({"name": "abc"})).is_empty());
    let other = try_compile_schema(&schema, CompileOptions::default()).unwrap();
    assert!(other.check(&invalid), "unknown formats remain annotations");

    let union = try_compile_schema(
        &json!({"anyOf": [
            {"type": "string", "format": "short-name"},
            {"type": "integer"}
        ]}),
        CompileOptions::default().with_format("short-name", |s| s.len() <= 3),
    )
    .unwrap();
    let coerced = union.coerce(&json!("1234"));
    assert_eq!(coerced, json!(1234));
    assert!(union.check(&coerced));

    let override_builtin = try_compile_schema(
        &json!({"format": "email"}),
        CompileOptions::default().with_format("email", |s| s == "local-only"),
    )
    .unwrap();
    assert!(override_builtin.check(&json!("local-only")));
    assert!(!override_builtin.check(&json!("a@example.com")));
    assert!(!override_builtin.errors(&json!("a@example.com")).is_empty());
}

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use pi_agent::{CompileOptions, try_compile_schema};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct Group {
    pattern: String,
    tests: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    data: String,
    expected: bool,
}

#[test]
fn ecma_patterns_match_upstream() {
    // Expected results from pi-schema@1.5.0 compile({type:"string", pattern},
    // {coerce:false}); fixtures require no Node runtime or external checkout.
    let groups: Vec<Group> =
        serde_json::from_str(include_str!("fixtures/ecma_regex.json")).unwrap();
    for group in groups {
        let validator = try_compile_schema(
            &json!({"type": "string", "pattern": group.pattern}),
            CompileOptions::default(),
        )
        .unwrap_or_else(|e| panic!("pattern {:?}: {e}", group.pattern));
        for case in group.tests {
            assert_eq!(
                validator.check(&json!(case.data)),
                case.expected,
                "pattern {:?}, input {:?}",
                group.pattern,
                case.data
            );
        }
    }
}

fn native_find(pattern: &str, text: &str, limit: usize) -> Result<bool, regress::MatchLimitError> {
    let re = regress::Regex::with_flags(pattern, "u").unwrap();
    re.find_utf16_bounded(&text.encode_utf16().collect::<Vec<_>>(), limit)
        .map(|m| m.is_some())
}

#[test]
fn budget_covers_search_loops_assertions_and_backreferences() {
    let long = "a".repeat(1_000);
    for pattern in [
        "^a+$",              // optimized greedy loop
        "^a+?b$",            // optimized lazy loop
        "z",                 // candidate search shares one budget
        "^(?=a+$)a",         // positive lookahead
        "^(?!a+b)a+$",       // negative lookahead cannot turn exhaustion into success
        "(?<=a+)b",          // lookbehind shares the same budget
        "^(a{100})\\1+$",    // capture comparison is charged before doing work
        "^(a?){100000000}$", // empty repeated body
    ] {
        assert!(
            native_find(pattern, &long, 50).is_err(),
            "{pattern:?} must exhaust its budget"
        );
    }
    // This budget reaches the backreference after the 100-character lookahead,
    // but cannot pay the additional 100-character capture comparison.
    assert!(native_find(r"^(?=(a{100}))\1$", &"a".repeat(100), 150).is_err());
    assert_eq!(
        native_find(r"^(?=(a{100}))\1$", &"a".repeat(100), 1_000),
        Ok(true)
    );
    assert_eq!(native_find("^a+$", &long, 10_000), Ok(true));
    assert_eq!(native_find("z", "abc", 100), Ok(false));
    assert!(native_find("", "", 0).is_err());
    let unicode_set = regress::Regex::with_flags("^.$", "v").unwrap();
    assert!(
        unicode_set
            .find_utf16_bounded(&"😀".encode_utf16().collect::<Vec<_>>(), 100)
            .unwrap()
            .is_some()
    );
    // Budget is per invocation, not stored in the cached regex.
    let re = regress::Regex::with_flags("^a+$", "u").unwrap();
    let text: Vec<_> = long.encode_utf16().collect();
    assert!(re.find_utf16_bounded(&text, 50).is_err());
    assert!(re.find_utf16_bounded(&text, 10_000).unwrap().is_some());
    assert!(re.find_utf16_bounded(&text, 50).is_err());
}

#[test]
fn budget_exhaustion_is_reported_by_schema_validation() {
    let validator = try_compile_schema(
        &json!({"type": "string", "pattern": "^(?!a+b)a+$"}),
        CompileOptions {
            regex_backtrack_limit: 50,
            ..CompileOptions::default()
        },
    )
    .unwrap();
    let input = json!("a".repeat(1_000));
    assert!(!validator.check(&input));
    assert!(
        validator
            .errors(&input)
            .iter()
            .any(|e| e.keyword == "pattern" && e.message.contains("limit exceeded"))
    );

    let properties = try_compile_schema(
        &json!({"type": "object", "patternProperties": {"^(a)?\\1$": {"type": "integer"}}}),
        CompileOptions::default(),
    )
    .unwrap();
    assert!(properties.check(&json!({"": 1, "aa": 2})));
    assert!(!properties.check(&json!({"": "invalid"})));
    let bounded_properties = try_compile_schema(
        &json!({"patternProperties": {"^a+$": {"type": "integer"}}}),
        CompileOptions {
            regex_backtrack_limit: 50,
            ..CompileOptions::default()
        },
    )
    .unwrap();
    let input = json!({"a".repeat(1_000): "invalid"});
    assert!(!bounded_properties.check(&input));
    assert!(
        bounded_properties
            .errors(&input)
            .iter()
            .any(|e| e.keyword == "patternProperties" && e.message.contains("limit exceeded"))
    );
}

#[test]
fn catastrophic_backtracking_child() {
    if std::env::var_os("PI_AGENT_NATIVE_REGEX_CHILD").is_none() {
        return;
    }
    let input = format!("{}!", "a".repeat(32));
    for pattern in [
        "^(a+)+$",
        "^(?=(a+)+b)a+$",
        "^(?!(a+)+b)a+$",
        "^((a+)b?)+\\1c$",
    ] {
        for limit in [1, 100_000] {
            assert!(native_find(pattern, &input, limit).is_err(), "{pattern:?}");
        }
    }
}

#[test]
fn catastrophic_backtracking_stops_synchronously() {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "catastrophic_backtracking_child", "--nocapture"])
        .env("PI_AGENT_NATIVE_REGEX_CHILD", "1")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "bounded matcher child failed: {status}");
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("native regex matching did not stop within its execution budget");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

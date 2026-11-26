use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

use pi_ai::UserContentBlock;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::diagnostics::result::inject_elapsed_ms;
use crate::diagnostics::volume_index::VolumeIdentity;

const MAX_BYTES: usize = 50 * 1024;

use super::grep_engine::{self, ContentMatcher, merge_and_limit_grep_rows, scan_reader_streaming};
use super::mft_index::{
    BuildFlight, BuildOutcome, BuildSlotPool, IndexBuildStatus, NameEdge, ObjectNode,
    SearchIndexManager, SlotAcquire, USN_REASON_FILE_CREATE, USN_REASON_FILE_DELETE,
    USN_REASON_RENAME_NEW_NAME, USN_REASON_RENAME_OLD_NAME, UsnEventV2, UsnReadError,
    VolumeFileIndex, parse_usn_read_buffer,
};
use super::win_volumes;
use super::{
    FffindInput, FffindOutput, FfgrepInput, FfgrepOutput, definitions, find_paths,
    find_paths_with_details, grep_files,
};
use crate::tools::path_guard::PathGuard;

struct TempWorkspace {
    root: PathBuf,
}

impl TempWorkspace {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("cst-fff-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let canonical = dir.canonicalize().unwrap();
        Self { root: canonical }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn root_str(&self) -> String {
        super::normalize_path(&self.root)
    }

    fn write(&self, rel: &str, content: &[u8]) {
        let p = self.path(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, content).unwrap();
    }

    fn guard(&self) -> PathGuard {
        PathGuard::new(&self.root).unwrap()
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn fffind_in(v: Value) -> FffindInput {
    serde_json::from_value(v).unwrap()
}

fn ffgrep_in(v: Value) -> FfgrepInput {
    serde_json::from_value(v).unwrap()
}

// ============================================================================
// V1: Independent Schemas, output_schema, structured_content, Validation Matrix
// ============================================================================

#[test]
fn v1_schema_snapshot_and_independent_fields() {
    let ws = TempWorkspace::new("v1-schema");
    let defs = definitions(ws.guard());
    assert_eq!(defs.len(), 2);

    let find_def = defs.iter().find(|d| d.name == "fffind").unwrap();
    let grep_def = defs.iter().find(|d| d.name == "ffgrep").unwrap();

    // fffind schema: required = ["path"], no context/literal/include.
    let find_params = &find_def.parameters;
    let find_req: Vec<&str> = find_params["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(find_req, vec!["path"]);
    let find_props = find_params["properties"].as_object().unwrap();
    for expected in [
        "path",
        "pattern",
        "kind",
        "ext",
        "exclude",
        "maxDepth",
        "modifiedWithinHours",
        "minSizeMiB",
        "maxSizeMiB",
        "sortBy",
        "caseSensitive",
        "limit",
        "timeoutSec",
    ] {
        assert!(
            find_props.contains_key(expected),
            "fffind schema missing property `{expected}`"
        );
    }
    assert_eq!(find_props["timeoutSec"]["type"], "integer");
    assert_eq!(find_props["timeoutSec"]["default"], 30);
    assert_eq!(find_props["timeoutSec"]["minimum"], 1);
    assert_eq!(find_props["timeoutSec"]["maximum"], 180);
    assert!(!find_req.contains(&"timeoutSec"));
    assert_eq!(fffind_in(json!({ "path": "." })).timeout_sec, 30);
    assert_eq!(
        fffind_in(json!({ "path": ".", "timeoutSec": null })).timeout_sec,
        30
    );
    for forbidden in ["context", "literal", "include"] {
        assert!(
            !find_props.contains_key(forbidden),
            "fffind schema must not expose `{forbidden}`"
        );
    }
    assert!(find_def.output_schema.is_some());

    // ffgrep schema: required = ["pattern", "path"], no kind/ext/sortBy/maxDepth/minSizeMiB.
    let grep_params = &grep_def.parameters;
    let grep_req: HashSet<&str> = grep_params["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(grep_req, HashSet::from(["pattern", "path"]));
    let grep_props = grep_params["properties"].as_object().unwrap();
    for expected in [
        "pattern",
        "path",
        "include",
        "exclude",
        "modifiedWithinHours",
        "literal",
        "caseSensitive",
        "context",
        "limit",
        "timeoutSec",
    ] {
        assert!(
            grep_props.contains_key(expected),
            "ffgrep schema missing property `{expected}`"
        );
    }
    assert_eq!(grep_props["timeoutSec"]["type"], "integer");
    assert_eq!(grep_props["timeoutSec"]["default"], 30);
    assert_eq!(grep_props["timeoutSec"]["minimum"], 1);
    assert_eq!(grep_props["timeoutSec"]["maximum"], 180);
    assert!(!grep_req.contains("timeoutSec"));
    assert_eq!(
        ffgrep_in(json!({ "pattern": "a", "path": "." })).timeout_sec,
        30
    );
    assert_eq!(
        ffgrep_in(json!({ "pattern": "a", "path": ".", "timeoutSec": null })).timeout_sec,
        30
    );
    for forbidden in [
        "kind",
        "ext",
        "maxDepth",
        "minSizeMiB",
        "maxSizeMiB",
        "sortBy",
    ] {
        assert!(
            !grep_props.contains_key(forbidden),
            "ffgrep schema must not expose `{forbidden}`"
        );
    }
    assert!(grep_def.output_schema.is_some());

    // Ensure FffindOutput and FfgrepOutput schemas round-trip cleanly.
    let find_out_schema = schemars::schema_for!(FffindOutput).to_value();
    let grep_out_schema = schemars::schema_for!(FfgrepOutput).to_value();
    assert!(find_out_schema["properties"]["root"].is_object());
    assert!(find_out_schema["properties"]["matches"].is_object());
    assert!(grep_out_schema["properties"]["root"].is_object());
    assert!(grep_out_schema["properties"]["matches"].is_object());
}

#[test]
fn v1_validation_rejects_invalid_combinations_and_boundaries() {
    let ws = TempWorkspace::new("v1-val");
    ws.write("sample.log", b"hello world\n");
    let guard = ws.guard();
    let cancel = CancellationToken::new();
    let root = ws.root_str();

    // Unknown fields rejected by deny_unknown_fields.
    assert!(serde_json::from_value::<FffindInput>(json!({"path": &root, "context": 1})).is_err());

    // timeoutSec bounds: 0 and 181 rejected; 1 and 180 accepted.
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "timeoutSec": 0})),
            &cancel
        )
        .is_err()
    );
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "timeoutSec": 181})),
            &cancel
        )
        .is_err()
    );
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "timeoutSec": 1})),
            &cancel
        )
        .is_ok()
    );
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "timeoutSec": 180})),
            &cancel
        )
        .is_ok()
    );
    assert!(
        grep_files(
            &guard,
            &ws.root,
            ffgrep_in(json!({"path": &root, "pattern": "hello", "timeoutSec": 0})),
            &cancel
        )
        .is_err()
    );
    assert!(
        grep_files(
            &guard,
            &ws.root,
            ffgrep_in(json!({"path": &root, "pattern": "hello", "timeoutSec": 181})),
            &cancel
        )
        .is_err()
    );
    assert!(
        grep_files(
            &guard,
            &ws.root,
            ffgrep_in(json!({"path": &root, "pattern": "hello", "timeoutSec": 60})),
            &cancel
        )
        .is_ok()
    );
    assert!(
        serde_json::from_value::<FffindInput>(json!({"path": &root, "literal": true})).is_err()
    );
    assert!(
        serde_json::from_value::<FfgrepInput>(
            json!({"pattern": "a", "path": &root, "ext": ["log"]})
        )
        .is_err()
    );
    // Missing required path fails deserialization.
    assert!(serde_json::from_value::<FffindInput>(json!({"pattern": "sample"})).is_err());
    assert!(serde_json::from_value::<FfgrepInput>(json!({"pattern": "hello"})).is_err());

    // Empty / whitespace-only pattern rejected for fffind.
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "pattern": ""})),
            &cancel
        )
        .is_err()
    );
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "pattern": "   \t "})),
            &cancel
        )
        .is_err()
    );

    // Over 4096 UTF-8 byte Chinese pattern rejected.
    let huge_cn = "中".repeat(1400); // 4200 UTF-8 bytes
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "pattern": &huge_cn})),
            &cancel
        )
        .is_err()
    );
    assert!(
        grep_files(
            &guard,
            &ws.root,
            ffgrep_in(json!({"path": &root, "pattern": &huge_cn})),
            &cancel
        )
        .is_err()
    );

    // Malformed glob rejected for fffind.
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "pattern": "[unclosed"})),
            &cancel
        )
        .is_err()
    );

    // kind="dir" conflicts with ext / minSizeMiB / maxSizeMiB / sortBy="size".
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "kind": "dir", "ext": ["log"]})),
            &cancel
        )
        .is_err()
    );
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "kind": "dir", "minSizeMiB": 1})),
            &cancel
        )
        .is_err()
    );
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "kind": "dir", "maxSizeMiB": 5})),
            &cancel
        )
        .is_err()
    );
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "kind": "dir", "sortBy": "size"})),
            &cancel
        )
        .is_err()
    );

    // `kind="dir"` + empty `ext: []` must still be rejected (P1-10).
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "kind": "dir", "ext": []})),
            &cancel
        )
        .is_err()
    );
    // `2^64 / 1048576 = 17592186044416` must be rejected cleanly instead of saturating to u64::MAX (P1-10).
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "minSizeMiB": 17_592_186_044_416.0})),
            &cancel
        )
        .is_err()
    );
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "maxSizeMiB": 17_592_186_044_416.0})),
            &cancel
        )
        .is_err()
    );

    // Numeric boundaries: limit, maxDepth, modifiedWithinHours, min/maxSizeMiB, context.
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "limit": 0})),
            &cancel
        )
        .is_err()
    );
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "limit": 1001})),
            &cancel
        )
        .is_err()
    );
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "maxDepth": 0})),
            &cancel
        )
        .is_err()
    );
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "maxDepth": 1025})),
            &cancel
        )
        .is_err()
    );
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "modifiedWithinHours": 0})),
            &cancel
        )
        .is_err()
    );
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "modifiedWithinHours": -1})),
            &cancel
        )
        .is_err()
    );
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "minSizeMiB": 5, "maxSizeMiB": 2})),
            &cancel
        )
        .is_err()
    );
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "ext": ["tar.gz"]})),
            &cancel
        )
        .is_err()
    );
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &root, "ext": ["*.log"]})),
            &cancel
        )
        .is_err()
    );

    // Single-file ffgrep rejects include / exclude, fffind single-file rejects maxDepth.
    let single_file = super::normalize_path(&ws.path("sample.log"));
    assert!(
        grep_files(
            &guard,
            &ws.root,
            ffgrep_in(json!({"path": &single_file, "pattern": "hello", "include": ["*.log"]})),
            &cancel
        )
        .is_err()
    );
    assert!(
        grep_files(
            &guard,
            &ws.root,
            ffgrep_in(json!({"path": &single_file, "pattern": "hello", "exclude": ["*.bak"]})),
            &cancel
        )
        .is_err()
    );
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &single_file, "maxDepth": 1})),
            &cancel
        )
        .is_err()
    );
    assert!(
        grep_files(
            &guard,
            &ws.root,
            ffgrep_in(json!({"path": &root, "pattern": "hello", "context": 21})),
            &cancel
        )
        .is_err()
    );
    // ffgrep rejects empty pattern and newline pattern.
    assert!(
        grep_files(
            &guard,
            &ws.root,
            ffgrep_in(json!({"path": &root, "pattern": ""})),
            &cancel
        )
        .is_err()
    );
    assert!(
        grep_files(
            &guard,
            &ws.root,
            ffgrep_in(json!({"path": &root, "pattern": "a\nb"})),
            &cancel
        )
        .is_err()
    );
    // Explicit null optional fields accepted via serde default.
    let ok = find_paths(
        &guard,
        &ws.root,
        fffind_in(json!({
            "path": &root,
            "pattern": null,
            "kind": null,
            "ext": null,
            "limit": null
        })),
        &cancel,
    )
    .unwrap();
    assert_eq!(ok["matches"].as_array().unwrap().len(), 1);
}

#[test]
fn v1_structured_content_and_elapsed_ms_stay_consistent_under_50kib_bound() {
    let ws = TempWorkspace::new("v1-struct");
    for i in 0..160 {
        ws.write(
            &format!("deep/folder_{i:03}/{}", "a".repeat(120)),
            b"content",
        );
    }
    let guard = ws.guard();
    let cancel = CancellationToken::new();
    let (model_val, details_val) = find_paths_with_details(
        &guard,
        &ws.root,
        fffind_in(json!({"path": ws.root_str(), "limit": 1000})),
        &cancel,
    )
    .unwrap();
    let tool_res =
        crate::diagnostics::result::structured_search_result(model_val, details_val).unwrap();
    let tool_res = inject_elapsed_ms(tool_res, 42);

    assert!(tool_res.structured_content.is_some());
    let sc = tool_res.structured_content.as_ref().unwrap();
    assert_eq!(sc["elapsedMs"], 42);
    assert_eq!(tool_res.details["elapsedMs"], 42);
    assert!(tool_res.details["method"].is_string());
    assert!(sc.get("method").is_none());

    let text = match &tool_res.content[0] {
        UserContentBlock::Text(t) => t.text.as_str(),
        _ => panic!("expected text block"),
    };
    assert!(text.len() <= MAX_BYTES);
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(&parsed, sc);
    assert!(parsed["root"].is_string());
}

// ============================================================================
// V2: Path Resolution, Wildcards, UNC, `.git`/`.state` Root Rejection
// ============================================================================

#[test]
fn v2_path_rules_literal_brackets_and_forbidden_roots() {
    let ws = TempWorkspace::new("v2-path");
    ws.write("[dir]/report.txt", b"ok\n");
    ws.write(".git/config", b"[core]\n");
    ws.write(".state/secret.txt", b"secret\n");
    let guard = ws.guard();
    let cancel = CancellationToken::new();

    // Literal `[dir]` is allowed as path (not treated as glob).
    let bracket_dir = super::normalize_path(&ws.path("[dir]"));
    let res = find_paths(
        &guard,
        &ws.root,
        fffind_in(json!({"path": &bracket_dir})),
        &cancel,
    )
    .unwrap();
    assert_eq!(res["matches"].as_array().unwrap().len(), 1);
    assert!(
        res["matches"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("/[dir]/report.txt")
    );

    // Embedded `*` or `?` in path rejected.
    let embedded_star = format!("{}/*.txt", ws.root_str());
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &embedded_star})),
            &cancel
        )
        .is_err()
    );
    assert!(
        grep_files(
            &guard,
            &ws.root,
            ffgrep_in(json!({"path": &embedded_star, "pattern": "ok"})),
            &cancel
        )
        .is_err()
    );
    // ffgrep rejects path="*".
    assert!(
        grep_files(
            &guard,
            &ws.root,
            ffgrep_in(json!({"path": "*", "pattern": "ok"})),
            &cancel
        )
        .is_err()
    );

    // UNC and drive-relative paths rejected on Windows.
    #[cfg(windows)]
    for bad in [r"\\server\share", "//server/share", r"\Windows", "C:foo"] {
        assert!(
            find_paths(&guard, &ws.root, fffind_in(json!({"path": bad})), &cancel).is_err(),
            "must reject {bad}"
        );
    }

    // `.git` and `.state` roots rejected.
    let git_root = super::normalize_path(&ws.path(".git"));
    let state_root = super::normalize_path(&ws.path(".state"));
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &git_root})),
            &cancel
        )
        .is_err()
    );
    assert!(
        find_paths(
            &guard,
            &ws.root,
            fffind_in(json!({"path": &state_root})),
            &cancel
        )
        .is_err()
    );
    assert!(
        grep_files(
            &guard,
            &ws.root,
            ffgrep_in(json!({"path": &git_root, "pattern": "core"})),
            &cancel
        )
        .is_err()
    );
}

// ============================================================================
// V3: fffind Filtering, Exact-With-Spaces, `.gitignore` Ignored, Sorting
// ============================================================================

#[test]
fn v3_fffind_filters_kind_ext_depth_size_sort_and_no_gitignore() {
    let ws = TempWorkspace::new("v3-find");
    ws.write(".gitignore", b"ignored_dir/\n*.mp4\n");
    ws.write("ignored_dir/X-op7418-2125331795998851072.mp4", &[1u8; 4096]);
    ws.write("sub/deep/My Video File.mp4", &[2u8; 2 * 1024 * 1024]);
    ws.write("sub/empty.log", b"");
    ws.write("sub/small.log", b"abc");
    ws.write(".git/objects/hidden.mp4", b"git-internal");
    std::fs::create_dir_all(ws.path("WeChat Files/UserA")).unwrap();

    let guard = ws.guard();
    let cancel = CancellationToken::new();
    let root = ws.root_str();

    // 1. `.gitignore` does NOT hide `.mp4` or `ignored_dir/`; `.git` IS skipped.
    let mp4s = find_paths(
        &guard,
        &ws.root,
        fffind_in(json!({"path": &root, "ext": [".MP4"]})),
        &cancel,
    )
    .unwrap();
    let paths: Vec<&str> = mp4s["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths.len(), 2);
    assert!(
        paths
            .iter()
            .any(|p| p.ends_with("X-op7418-2125331795998851072.mp4"))
    );
    assert!(paths.iter().any(|p| p.ends_with("My Video File.mp4")));
    assert!(!paths.iter().any(|p| p.contains(".git")));

    // 2. Exact filename containing spaces matches before whitespace tokenization.
    let space_hit = find_paths(
        &guard,
        &ws.root,
        fffind_in(json!({"path": &root, "pattern": "My Video File.mp4"})),
        &cancel,
    )
    .unwrap();
    assert_eq!(space_hit["matches"].as_array().unwrap().len(), 1);
    assert!(
        space_hit["matches"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("My Video File.mp4")
    );

    // 3. `kind: "dir"` returns directories with `sizeBytes: null`.
    let dirs = find_paths(
        &guard,
        &ws.root,
        fffind_in(json!({"path": &root, "pattern": "WeChat", "kind": "dir"})),
        &cancel,
    )
    .unwrap();
    let dir_rows = dirs["matches"].as_array().unwrap();
    assert!(!dir_rows.is_empty());
    assert!(
        dir_rows[0]["path"]
            .as_str()
            .unwrap()
            .ends_with("/WeChat Files")
    );
    assert_eq!(dir_rows[0]["kind"], "dir");
    assert!(dir_rows[0]["sizeBytes"].is_null());

    // 4. `maxDepth: 1` only returns direct children.
    let shallow = find_paths(
        &guard,
        &ws.root,
        fffind_in(json!({"path": &root, "kind": "any", "maxDepth": 1})),
        &cancel,
    )
    .unwrap();
    assert!(
        shallow["matches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| !m["path"].as_str().unwrap().contains("UserA"))
    );

    // 5. `maxSizeMiB: 0` matches ONLY 0-byte files.
    let zero_only = find_paths(
        &guard,
        &ws.root,
        fffind_in(json!({"path": &root, "maxSizeMiB": 0})),
        &cancel,
    )
    .unwrap();
    let zero_rows = zero_only["matches"].as_array().unwrap();
    assert_eq!(zero_rows.len(), 1);
    assert!(
        zero_rows[0]["path"]
            .as_str()
            .unwrap()
            .ends_with("empty.log")
    );
    assert_eq!(zero_rows[0]["sizeBytes"], 0);

    // 6. `minSizeMiB: 1` + `sortBy: "size"` returns the 2 MiB file.
    let big = find_paths(
        &guard,
        &ws.root,
        fffind_in(json!({"path": &root, "minSizeMiB": 1, "sortBy": "size"})),
        &cancel,
    )
    .unwrap();
    let big_rows = big["matches"].as_array().unwrap();
    assert_eq!(big_rows.len(), 1);
    assert_eq!(big_rows[0]["sizeBytes"], 2 * 1024 * 1024);

    // 7. `exclude` with trailing `/` prunes directory.
    let excl = find_paths(
        &guard,
        &ws.root,
        fffind_in(json!({"path": &root, "ext": ["mp4"], "exclude": ["ignored_dir/"]})),
        &cancel,
    )
    .unwrap();
    assert_eq!(excl["matches"].as_array().unwrap().len(), 1);
    assert!(
        excl["matches"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("My Video File.mp4")
    );
}

// ============================================================================
// V4: ffgrep 1-Byte Chunked Streaming, Encodings, NUL Rollback, Long Lines, Budget
// ============================================================================

#[test]
fn v4_streaming_decoder_1byte_chunks_utf8_utf16_gb18030_and_crlf() {
    let deadline = Instant::now() + Duration::from_secs(10);
    let cancel = CancellationToken::new();

    // 1. UTF-8 multi-byte (2/3/4-byte + emoji) + CRLF fed 1 byte at a time.
    let utf8_text = "前导\r\né中文🚀NEEDLE\r\n结尾\r\n";
    let matcher = ContentMatcher::build("🚀NEEDLE", true, true).unwrap();
    let budget = AtomicUsize::new(1024 * 1024);
    let out = scan_reader_streaming(
        Cursor::new(utf8_text.as_bytes()),
        "C:/test/u8.txt",
        utf8_text.len() as u64,
        &budget,
        &matcher,
        1,
        10,
        deadline,
        &cancel,
        1, // 1-byte chunk stress test!
    )
    .unwrap();
    assert!(!out.is_binary);
    let (rows, _) = merge_and_limit_grep_rows(out.rows, 10);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].line, 1);
    assert!(!rows[0].matched);
    assert_eq!(rows[1].line, 2);
    assert!(rows[1].matched);
    assert_eq!(rows[1].text, "é中文🚀NEEDLE");
    assert_eq!(rows[2].line, 3);
    assert!(!rows[2].matched);

    // 2. UTF-16 LE BOM with surrogate pair (🚀) fed 1 byte at a time.
    let mut u16le = vec![0xFFu8, 0xFEu8];
    for u in "line1\r\n日志🚀HIT\r\nline3\r\n".encode_utf16() {
        u16le.extend_from_slice(&u.to_le_bytes());
    }
    let m16 = ContentMatcher::build("🚀HIT", true, true).unwrap();
    let budget = AtomicUsize::new(1024 * 1024);
    let out = scan_reader_streaming(
        Cursor::new(&u16le),
        "C:/test/u16le.log",
        u16le.len() as u64,
        &budget,
        &m16,
        0,
        10,
        deadline,
        &cancel,
        1,
    )
    .unwrap();
    assert!(!out.is_binary);
    assert_eq!(out.rows.len(), 1);
    assert_eq!(out.rows[0].line, 2);
    assert_eq!(out.rows[0].text, "日志🚀HIT");

    // 3. UTF-16 BE BOM fed 1 byte at a time.
    let mut u16be = vec![0xFEu8, 0xFFu8];
    for u in "中文BE_HIT\n".encode_utf16() {
        u16be.extend_from_slice(&u.to_be_bytes());
    }
    let mbe = ContentMatcher::build("中文BE_HIT", true, true).unwrap();
    let budget = AtomicUsize::new(1024 * 1024);
    let out = scan_reader_streaming(
        Cursor::new(&u16be),
        "C:/test/u16be.log",
        u16be.len() as u64,
        &budget,
        &mbe,
        0,
        10,
        deadline,
        &cancel,
        1,
    )
    .unwrap();
    assert!(!out.is_binary);
    assert_eq!(out.rows.len(), 1);
    assert_eq!(out.rows[0].text, "中文BE_HIT");

    // 4. ASCII prefix followed by late GB18030 Chinese (no BOM) fed 1 byte at a time.
    let (gb_bytes, _, _) = encoding_rs::GB18030.encode("ASCII_HEADER\n错误代码：蓝屏核心\n");
    let mgb = ContentMatcher::build("蓝屏核心", true, true).unwrap();
    let budget = AtomicUsize::new(1024 * 1024);
    let out = scan_reader_streaming(
        Cursor::new(gb_bytes.as_ref()),
        "C:/test/gb.log",
        gb_bytes.len() as u64,
        &budget,
        &mgb,
        1,
        10,
        deadline,
        &cancel,
        1,
    )
    .unwrap();
    assert!(!out.is_binary);
    let (rows, _) = merge_and_limit_grep_rows(out.rows, 10);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].line, 2);
    assert!(rows[1].matched);
    assert!(rows[1].text.contains("蓝屏核心"));
}

#[test]
fn v4_streaming_long_lines_nul_rollback_budget_truncation_and_match_priority() {
    let deadline = Instant::now() + Duration::from_secs(10);
    let cancel = CancellationToken::new();

    // 1. Oversized > 8 KiB line skipped while preserving exact physical line numbers & no context bridging.
    let oversized = format!("ctx_before\n{}\nNEEDLE_AFTER\n", "x".repeat(10 * 1024));
    let m = ContentMatcher::build("NEEDLE_AFTER", true, true).unwrap();
    let budget = AtomicUsize::new(1024 * 1024);
    let out = scan_reader_streaming(
        Cursor::new(oversized.as_bytes()),
        "C:/test/long.log",
        oversized.len() as u64,
        &budget,
        &m,
        1,
        10,
        deadline,
        &cancel,
        17,
    )
    .unwrap();
    assert!(out.hit_line_limit);
    let (rows, _) = merge_and_limit_grep_rows(out.rows, 10);
    // Line 2 was dropped; before-context must NOT pull line 1 across the dropped line 2!
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].line, 3);
    assert!(rows[0].matched);

    // 2. Late-chunk NUL triggers whole-file binary rollback (even if earlier chunk matched).
    let mut late_nul = b"MATCH_EARLY\n".to_vec();
    late_nul.extend_from_slice(&[b'a'; 128]);
    late_nul.push(0);
    let m_early = ContentMatcher::build("MATCH_EARLY", true, true).unwrap();
    let budget = AtomicUsize::new(1024 * 1024);
    let out = scan_reader_streaming(
        Cursor::new(&late_nul),
        "C:/test/bin.log",
        late_nul.len() as u64,
        &budget,
        &m_early,
        0,
        10,
        deadline,
        &cancel,
        32,
    )
    .unwrap();
    assert!(out.is_binary);
    assert!(out.rows.is_empty());

    // 3. Budget truncation cuts mid-line: must NOT falsely match `$` on truncated fragment.
    let truncated_src = b"prefix_only_no_newline_here";
    let m_end = ContentMatcher::build("prefix$", false, true).unwrap();
    let small_budget = AtomicUsize::new(6); // Cuts exactly after "prefix"
    let out = scan_reader_streaming(
        Cursor::new(truncated_src.as_slice()),
        "C:/test/trunc.log",
        truncated_src.len() as u64,
        &small_budget,
        &m_end,
        0,
        10,
        deadline,
        &cancel,
        4,
    )
    .unwrap();
    assert!(out.budget_truncated);
    assert!(
        out.rows.is_empty(),
        "truncated tail must not falsely match `$`"
    );

    // 4. `limit` prioritizes `matched: true` rows over context rows when `context >= limit`.
    let lines = "ctx1\nctx2\nHIT_ONE\nctx3\nctx4\nHIT_TWO\nctx5\n";
    let m_hit = ContentMatcher::build("HIT_", true, true).unwrap();
    let budget = AtomicUsize::new(1024 * 1024);
    let out = scan_reader_streaming(
        Cursor::new(lines.as_bytes()),
        "C:/test/prio.log",
        lines.len() as u64,
        &budget,
        &m_hit,
        2,
        2, // limit = 2, context = 2 -> must keep BOTH matched lines instead of ctx1/ctx2!
        deadline,
        &cancel,
        16,
    )
    .unwrap();
    let (rows, trunc) = merge_and_limit_grep_rows(out.rows, 2);
    assert!(trunc);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.matched));
    assert_eq!(rows[0].line, 3);
    assert_eq!(rows[1].line, 6);
}

#[test]
fn v4_ffgrep_regex_default_case_insensitive_and_single_file_over_10mib() {
    let ws = TempWorkspace::new("v4-big");
    ws.write("dir/case.log", b"ErrorCode=0x1A\n");
    // Build an 11 MiB log file with a hit near the end.
    let mut big = vec![b'a'; 11 * 1024 * 1024];
    for i in (100..big.len()).step_by(101) {
        big[i] = b'\n';
    }
    big.extend_from_slice(b"\nFINAL_STREAM_HIT_999\n");
    ws.write("dir/big11m.log", &big);

    let guard = ws.guard();
    let cancel = CancellationToken::new();

    // Regex omitting caseSensitive defaults to case-insensitive even with `\S` (uppercase escape!).
    let re_res = grep_files(
        &guard,
        &ws.root,
        ffgrep_in(json!({
            "path": super::normalize_path(&ws.path("dir")),
            "pattern": r"errorcode=\S+",
            "include": ["*.log"]
        })),
        &cancel,
    )
    .unwrap();
    assert_eq!(re_res["matches"].as_array().unwrap().len(), 1);
    assert_eq!(re_res["skippedLargeFiles"], 1);
    assert_eq!(re_res["skipped"], 0);

    // Direct single-file ffgrep streams through the 11 MiB file and finds the trailing hit!
    let single_res = grep_files(
        &guard,
        &ws.root,
        ffgrep_in(json!({
            "path": super::normalize_path(&ws.path("dir/big11m.log")),
            "pattern": "FINAL_STREAM_HIT_999",
            "literal": true
        })),
        &cancel,
    )
    .unwrap();
    let matches = single_res["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert!(
        matches[0]["text"]
            .as_str()
            .unwrap()
            .contains("FINAL_STREAM_HIT_999")
    );
    assert_eq!(single_res["truncated"], false);
}

// ============================================================================
// V5: Non-Leaking Policy Denials & Approved Top-K Invariant
// ============================================================================

#[test]
fn v5_policy_denied_entries_never_leak_counts_or_displace_legitimate_limit() {
    let ws = TempWorkspace::new("v5-sec");
    ws.write("data/config/auth.json", b"TOP_SECRET_KEY");
    ws.write(".state/hidden_target.txt", b"TOP_SECRET_KEY");
    // Hard link alias to credential with a high-priority exact-match name!
    std::fs::hard_link(ws.path("data/config/auth.json"), ws.path("target.txt")).unwrap();
    ws.write("sub/target.txt", b"PUBLIC_OK");

    let guard = ws.guard();
    let cancel = CancellationToken::new();

    // Ask for limit=1 matching "target.txt":
    // The root-level hard-link `target.txt` has a HIGHER relevance score than `sub/target.txt`,
    // yet it MUST be rejected without displacing `sub/target.txt`, and `skipped`/`truncated` MUST stay 0/false!
    let res = find_paths(
        &guard,
        &ws.root,
        fffind_in(json!({
            "path": ws.root_str(),
            "pattern": "target.txt",
            "limit": 1
        })),
        &cancel,
    )
    .unwrap();
    let matches = res["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert!(
        matches[0]["path"]
            .as_str()
            .unwrap()
            .ends_with("/sub/target.txt")
    );
    assert_eq!(
        res["skipped"], 0,
        "policy denials must never increment skipped"
    );
    assert_eq!(
        res["truncated"], false,
        "policy denials must never cause false truncation"
    );

    // Same non-leaking check for ffgrep.
    let gres = grep_files(
        &guard,
        &ws.root,
        ffgrep_in(json!({
            "path": ws.root_str(),
            "pattern": "TOP_SECRET_KEY",
            "literal": true
        })),
        &cancel,
    )
    .unwrap();
    assert!(gres["matches"].as_array().unwrap().is_empty());
    assert_eq!(
        gres["skipped"], 0,
        "ffgrep policy denials must never increment skipped"
    );
}

// ============================================================================
// V6 & V7: Offline VolumeFileIndex (64-bit FileRef, Hard Links, USN V2, TreeGen, LRU/TTL)
// ============================================================================

fn make_test_volume_index(drive: &str, est_bytes_override: Option<usize>) -> VolumeFileIndex {
    let root_ref: u64 = (5u64 << 48) | 5;
    let dir_a_ref: u64 = (1u64 << 48) | 100;
    let dir_b_ref: u64 = (1u64 << 48) | 101;
    let git_dir_ref: u64 = (1u64 << 48) | 102;
    let file_ref: u64 = (7u64 << 48) | 200;

    let mut name_arena = Vec::new();
    let mut push_name = |s: &str| -> (u32, u16) {
        let off = name_arena.len() as u32;
        name_arena.extend_from_slice(s.as_bytes());
        (off, s.len() as u16)
    };

    let (a_off, a_len) = push_name("DirA");
    let (b_off, b_len) = push_name("DirB");
    let (g_off, g_len) = push_name(".git");
    let (f1_off, f1_len) = push_name("link1.mp4");
    let (f2_off, f2_len) = push_name("link2.mp4");

    let objects = vec![
        ObjectNode {
            file_ref: root_ref,
            size_bytes: 0,
            mtime_secs: 1000,
            flags: 1, // FLAG_IS_DIR
            edge_count: 0,
            first_edge: u32::MAX,
        },
        ObjectNode {
            file_ref: dir_a_ref,
            size_bytes: 0,
            mtime_secs: 1000,
            flags: 1,
            edge_count: 1,
            first_edge: 0,
        },
        ObjectNode {
            file_ref: dir_b_ref,
            size_bytes: 0,
            mtime_secs: 1000,
            flags: 1,
            edge_count: 1,
            first_edge: 1,
        },
        ObjectNode {
            file_ref: git_dir_ref,
            size_bytes: 0,
            mtime_secs: 1000,
            flags: 1,
            edge_count: 1,
            first_edge: 2,
        },
        ObjectNode {
            file_ref,
            size_bytes: 2048,
            mtime_secs: 1000,
            flags: 0,
            edge_count: 2,
            first_edge: 3,
        },
    ];
    let edges = vec![
        NameEdge {
            parent_ref: root_ref,
            obj_idx: 1,
            name_offset: a_off,
            name_len: a_len,
            namespace: 1,
            flags: 0,
        },
        NameEdge {
            parent_ref: root_ref,
            obj_idx: 2,
            name_offset: b_off,
            name_len: b_len,
            namespace: 1,
            flags: 0,
        },
        NameEdge {
            parent_ref: root_ref,
            obj_idx: 3,
            name_offset: g_off,
            name_len: g_len,
            namespace: 1,
            flags: 0,
        },
        // Dual hard links to the same 64-bit FileReference in DirA and DirB!
        NameEdge {
            parent_ref: dir_a_ref,
            obj_idx: 4,
            name_offset: f1_off,
            name_len: f1_len,
            namespace: 1,
            flags: 0,
        },
        NameEdge {
            parent_ref: dir_b_ref,
            obj_idx: 4,
            name_offset: f2_off,
            name_len: f2_len,
            namespace: 1,
            flags: 0,
        },
    ];
    let mut ref_to_obj = HashMap::new();
    for (i, o) in objects.iter().enumerate() {
        ref_to_obj.insert(o.file_ref, i as u32);
    }
    VolumeFileIndex::new_for_test(
        drive.to_owned(),
        VolumeIdentity {
            volume_guid: Some(format!("guid-{drive}")),
            serial: 1,
            filesystem: "NTFS".into(),
            root_created: 0,
        },
        99,
        100,
        root_ref,
        objects,
        edges,
        name_arena,
        ref_to_obj,
        est_bytes_override,
    )
}

#[test]
fn v6_usn_v2_buffer_parser_valid_v2_and_rejects_v3() {
    // Build a valid USN V2 record buffer.
    let name_utf16: Vec<u16> = "demo.mp4".encode_utf16().collect();
    let name_bytes_len = name_utf16.len() * 2;
    let rec_len = (60 + name_bytes_len + 7) & !7;
    let mut buf = vec![0u8; 8 + rec_len];
    buf[0..8].copy_from_slice(&512i64.to_le_bytes()); // next_usn
    buf[8..12].copy_from_slice(&(rec_len as u32).to_le_bytes());
    buf[12..14].copy_from_slice(&2u16.to_le_bytes()); // major = 2
    buf[16..24].copy_from_slice(&((3u64 << 48) | 42).to_le_bytes()); // file_ref
    buf[24..32].copy_from_slice(&((5u64 << 48) | 5).to_le_bytes()); // parent_ref
    buf[32..40].copy_from_slice(&256i64.to_le_bytes()); // usn
    buf[48..52].copy_from_slice(&USN_REASON_FILE_CREATE.to_le_bytes());
    buf[60..64].copy_from_slice(&0x20u32.to_le_bytes()); // file_attributes
    buf[64..66].copy_from_slice(&(name_bytes_len as u16).to_le_bytes());
    buf[66..68].copy_from_slice(&60u16.to_le_bytes());
    for (i, u) in name_utf16.iter().enumerate() {
        buf[8 + 60 + i * 2..8 + 60 + i * 2 + 2].copy_from_slice(&u.to_le_bytes());
    }

    let (next_usn, events) = parse_usn_read_buffer(&buf).unwrap();
    assert_eq!(next_usn, 512);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].name_utf8, "demo.mp4");
    assert_eq!(events[0].file_ref, (3u64 << 48) | 42);
    assert!(events[0].name_lossless);

    // Major version 3 must return UnsupportedVersion (never silently truncate 128-bit IDs).
    buf[12..14].copy_from_slice(&3u16.to_le_bytes());
    assert!(matches!(
        parse_usn_read_buffer(&buf),
        Err(UsnReadError::UnsupportedVersion)
    ));
}

#[test]
fn v6_usn_events_sequence_reuse_hardlink_delete_dir_move_into_git_and_half_rename() {
    let mut idx = make_test_volume_index("C:", None);
    let root_ref = idx.root_ref;
    let dir_a_ref = (1u64 << 48) | 100;
    let dir_b_ref = (1u64 << 48) | 101;
    let git_dir_ref = (1u64 << 48) | 102;
    let file_ref = (7u64 << 48) | 200;

    // Deleting ONLY link1 keeps the object alive via link2!
    idx.apply_usn_events(
        &[UsnEventV2 {
            file_ref,
            parent_ref: dir_a_ref,
            usn: 110,
            reason: USN_REASON_FILE_DELETE,
            file_attributes: 0x20,
            name_utf8: "link1.mp4".into(),
            name_lossless: true,
        }],
        110,
    )
    .unwrap();
    assert!(idx.ref_to_obj.contains_key(&file_ref));
    assert_eq!(idx.live_objects, 5);

    // MFT slot 200 reused with NEW sequence 8 creates a distinct 64-bit object!
    let reused_ref = (8u64 << 48) | 200;
    idx.apply_usn_events(
        &[UsnEventV2 {
            file_ref: reused_ref,
            parent_ref: dir_a_ref,
            usn: 120,
            reason: USN_REASON_FILE_CREATE,
            file_attributes: 0x20,
            name_utf8: "new_seq.mp4".into(),
            name_lossless: true,
        }],
        120,
    )
    .unwrap();
    assert!(idx.ref_to_obj.contains_key(&file_ref));
    assert!(idx.ref_to_obj.contains_key(&reused_ref));
    assert_ne!(idx.ref_to_obj[&file_ref], idx.ref_to_obj[&reused_ref]);

    // Move DirB inside `.git`: DirB (and its child `link2.mp4`) becomes unreachable via tree_gen!
    assert!(idx.is_dir_reachable_for_test(2)); // DirB currently reachable
    idx.apply_usn_events(
        &[
            UsnEventV2 {
                file_ref: dir_b_ref,
                parent_ref: root_ref,
                usn: 130,
                reason: USN_REASON_RENAME_OLD_NAME,
                file_attributes: 0x10,
                name_utf8: "DirB".into(),
                name_lossless: true,
            },
            UsnEventV2 {
                file_ref: dir_b_ref,
                parent_ref: git_dir_ref,
                usn: 131,
                reason: USN_REASON_RENAME_NEW_NAME,
                file_attributes: 0x10,
                name_utf8: "DirB".into(),
                name_lossless: true,
            },
        ],
        131,
    )
    .unwrap();
    assert!(
        !idx.is_dir_reachable_for_test(2),
        "directory moved under .git must be pruned via tree_gen"
    );

    // Half-rename (OLD without NEW) must return Err(()) to trigger safe full rebuild.
    let half = idx.apply_usn_events(
        &[UsnEventV2 {
            file_ref: dir_a_ref,
            parent_ref: root_ref,
            usn: 140,
            reason: USN_REASON_RENAME_OLD_NAME,
            file_attributes: 0x10,
            name_utf8: "DirA".into(),
            name_lossless: true,
        }],
        140,
    );
    assert!(half.is_err());
}

#[test]
fn v7_search_index_manager_lru_eviction_arc_reader_ledger_and_idle_ttl() {
    let mgr = SearchIndexManager::new(Duration::from_millis(40), 100 * 1024 * 1024);
    let now = Instant::now();
    let c_idx = make_test_volume_index("C:", Some(60 * 1024 * 1024));
    let d_idx = make_test_volume_index("D:", Some(60 * 1024 * 1024));

    let c_reader = mgr
        .insert_for_test(c_idx, now - Duration::from_secs(5))
        .expect("C: fits in 100 MiB");
    assert_eq!(mgr.cached_bytes(), 60 * 1024 * 1024);
    assert_eq!(mgr.reserved_bytes(), 60 * 1024 * 1024);

    // While an in-flight reader STILL holds `c_reader`, inserting D: (60 MiB) evicts C: from the
    // cache map, YET `c_reader`'s 60 MiB remains charged against the ledger -> D: is rejected!
    assert!(mgr.insert_for_test(d_idx, now).is_none());
    assert_eq!(mgr.cached_bytes(), 0);
    assert_eq!(mgr.reserved_bytes(), 60 * 1024 * 1024);

    // Once the in-flight reader drops `c_reader`, its reservation releases immediately and D: succeeds!
    drop(c_reader);
    assert_eq!(mgr.reserved_bytes(), 0);
    let d_idx2 = make_test_volume_index("D:", Some(60 * 1024 * 1024));
    let _d_arc = mgr
        .insert_for_test(d_idx2, now)
        .expect("D: fits after drop");
    drop(_d_arc);
    assert_eq!(mgr.cached_bytes(), 60 * 1024 * 1024);

    // After idle TTL elapses, purge_idle releases cached memory & ledger reservation to 0.
    std::thread::sleep(Duration::from_millis(60));
    mgr.purge_idle();
    assert_eq!(mgr.cached_bytes(), 0);
    assert_eq!(mgr.reserved_bytes(), 0);
}

#[test]
fn v1_and_v4_sol_review_regressions_cr_preservation_50kib_match_priority_and_usn_tail() {
    // 1. P2-1: CR preservation (double CR before LF strips only 1 CR; bare CR at EOF preserved).
    let deadline = Instant::now() + Duration::from_secs(10);
    let cancel = CancellationToken::new();
    let src = b"a\r\r\nb\r";
    let m = ContentMatcher::build("a\r", true, true).unwrap();
    let budget = AtomicUsize::new(1024);
    let out = scan_reader_streaming(
        Cursor::new(src.as_slice()),
        "C:/test/cr.txt",
        src.len() as u64,
        &budget,
        &m,
        1,
        10,
        deadline,
        &cancel,
        1,
    )
    .unwrap();
    let (rows, _) = merge_and_limit_grep_rows(out.rows, 10);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].text, "a\r");
    assert!(rows[0].matched);
    assert_eq!(rows[1].text, "b\r");

    // 2. P1-9: 50 KiB atomic bound drops context rows before matched rows & rejects oversized root.
    let mut map = serde_json::Map::new();
    map.insert("root".into(), json!("C:/logs"));
    map.insert("truncated".into(), json!(false));
    map.insert("skipped".into(), json!(0));
    let big_ctx = "c".repeat(30_000);
    let hit_line = "MATCH_IMPORTANT".to_owned();
    map.insert(
        "matches".into(),
        json!([
            {"path": "C:/logs/a.log", "line": 1, "text": &big_ctx, "matched": false},
            {"path": "C:/logs/a.log", "line": 2, "text": &big_ctx, "matched": false},
            {"path": "C:/logs/a.log", "line": 3, "text": &hit_line, "matched": true},
        ]),
    );
    let bounded = crate::diagnostics::result::enforce_atomic_matches_bound(&mut map).unwrap();
    assert!(bounded.len() <= MAX_BYTES);
    assert!(
        map["matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["matched"] == true),
        "matched row must survive context trimming under 50 KiB bound"
    );

    let mut huge_root = serde_json::Map::new();
    huge_root.insert("root".into(), json!("x".repeat(60_000)));
    huge_root.insert("matches".into(), json!([]));
    assert!(crate::diagnostics::result::enforce_atomic_matches_bound(&mut huge_root).is_err());

    // 3. P1-5: Truncated trailing USN bytes must fail with IoFailed (never silently break).
    let truncated_usn = vec![0u8; 20]; // 8-byte next_usn + 12 truncated tail bytes (< 60)
    assert!(matches!(
        parse_usn_read_buffer(&truncated_usn),
        Err(UsnReadError::IoFailed)
    ));
}

#[test]
fn v7_mft_slot_pool_queues_five_volumes_respects_cancel_and_evicts_idle_lru() {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    // 1. 5 concurrent volumes queue cleanly on a 2-slot pool without dropping/falling back.
    let pool = BuildSlotPool::new(2);
    let cancel = CancellationToken::new();
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_seen = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    std::thread::scope(|scope| {
        for _ in 0..5 {
            let p = Arc::clone(&pool);
            let c = cancel.clone();
            let cur = Arc::clone(&in_flight);
            let peak = Arc::clone(&max_seen);
            let done = Arc::clone(&completed);
            scope.spawn(move || {
                let SlotAcquire::Acquired(_permit) = p.acquire_bounded(Duration::from_secs(5), &c)
                else {
                    panic!("queued volume must acquire slot");
                };
                let now = cur.fetch_add(1, Ordering::AcqRel) + 1;
                peak.fetch_max(now, Ordering::AcqRel);
                std::thread::sleep(Duration::from_millis(25));
                cur.fetch_sub(1, Ordering::AcqRel);
                done.fetch_add(1, Ordering::AcqRel);
            });
        }
    });
    assert_eq!(completed.load(Ordering::Acquire), 5);
    assert!(max_seen.load(Ordering::Acquire) <= 2);

    // 2. Cancellation wakes a queued waiter promptly while slots are busy; wedged slots time out.
    let SlotAcquire::Acquired(_p1) = pool.acquire_bounded(Duration::from_secs(1), &cancel) else {
        panic!("slot 1");
    };
    let SlotAcquire::Acquired(_p2) = pool.acquire_bounded(Duration::from_secs(1), &cancel) else {
        panic!("slot 2");
    };
    let cancel_waiter = CancellationToken::new();
    let cw_clone = cancel_waiter.clone();
    let p_clone = Arc::clone(&pool);
    let handle = std::thread::spawn(move || {
        matches!(
            p_clone.acquire_bounded(Duration::from_secs(5), &cw_clone),
            SlotAcquire::Cancelled
        )
    });
    std::thread::sleep(Duration::from_millis(20));
    cancel_waiter.cancel();
    assert!(handle.join().unwrap());

    // Wedged slots: 3rd waiter hits bounded queue timeout instead of hanging forever.
    let t_wedged = Instant::now();
    assert!(matches!(
        pool.acquire_bounded(Duration::from_millis(50), &cancel),
        SlotAcquire::TimedOut
    ));
    assert!(t_wedged.elapsed() < Duration::from_secs(1));
    drop(_p1);
    drop(_p2);

    // 3. BuildFlight queue wait time does NOT consume vol_budget, is independently bounded by
    // queue_budget, and USN + cold-build work accumulates without reset.
    let flight = Arc::new(BuildFlight::new());
    let f_bg = Arc::clone(&flight);
    let dummy_idx = Arc::new(make_test_volume_index("E:", Some(1024)));
    let dummy_for_bg = Arc::clone(&dummy_idx);
    std::thread::spawn(move || {
        // Queue for 140ms (longer than 90ms vol_budget, shorter than 400ms queue_budget).
        std::thread::sleep(Duration::from_millis(140));
        f_bg.mark_work_started(Instant::now());
        std::thread::sleep(Duration::from_millis(25));
        f_bg.finish(BuildOutcome::Ready(
            dummy_for_bg,
            IndexBuildStatus::ColdBuilt,
        ));
    });
    let (outcome, remaining) = flight.wait_with_budget(
        Duration::from_millis(90),
        Duration::from_millis(400),
        &cancel,
    );
    assert!(
        matches!(outcome, BuildOutcome::Ready(_, _)),
        "queue wait before work_started must not trigger timeout"
    );
    assert!(remaining > Duration::ZERO && remaining <= Duration::from_millis(90));

    // Queue timeout on wedged flight returns Fallback with FULL vol_budget preserved!
    let wedged_flight = BuildFlight::new();
    let (q_out, q_rem) = wedged_flight.wait_with_budget(
        Duration::from_millis(300),
        Duration::from_millis(50),
        &cancel,
    );
    assert!(matches!(q_out, BuildOutcome::Fallback));
    assert_eq!(q_rem, Duration::from_millis(300));

    // Cumulative active work: USN sync (60ms) -> queue pause (80ms) -> cold build (60ms)
    // exceeds 100ms vol_budget even though neither phase alone exceeded 100ms!
    let cum_flight = Arc::new(BuildFlight::new());
    let cf_bg = Arc::clone(&cum_flight);
    std::thread::spawn(move || {
        cf_bg.mark_work_started(Instant::now());
        std::thread::sleep(Duration::from_millis(60));
        cf_bg.pause_work_for_queue(Instant::now());
        std::thread::sleep(Duration::from_millis(80));
        cf_bg.mark_work_started(Instant::now());
        std::thread::sleep(Duration::from_millis(60));
        cf_bg.finish(BuildOutcome::Fallback);
    });
    let (cum_out, cum_rem) = cum_flight.wait_with_budget(
        Duration::from_millis(100),
        Duration::from_millis(400),
        &cancel,
    );
    assert!(
        matches!(cum_out, BuildOutcome::TimedOut),
        "USN + cold build active work must accumulate without reset"
    );
    assert_eq!(cum_rem, Duration::ZERO);

    // 4. reserve_with_eviction + same-volume stale generation eviction (with & without pinned reader).
    let mgr = SearchIndexManager::new(Duration::from_secs(60), 100 * 1024 * 1024);
    let now = Instant::now();
    let c_idx = make_test_volume_index("C:", Some(50 * 1024 * 1024));
    let d_idx = make_test_volume_index("D:", Some(40 * 1024 * 1024));
    let c_id = c_idx.identity.clone();
    let c_journal = c_idx.journal_id;
    let c_cursor = c_idx.applied_cursor;
    let c_arc = mgr
        .insert_for_test(c_idx, now - Duration::from_secs(10))
        .unwrap();
    let d_arc = mgr
        .insert_for_test(d_idx, now - Duration::from_secs(2))
        .unwrap();
    // Evicting stale C: while pinned removes it from cache map but keeps ledger bytes charged until drop.
    mgr.evict_stale_volume_generation("C:", &c_id, c_journal, c_cursor);
    assert_eq!(mgr.cached_bytes(), 40 * 1024 * 1024);
    assert_eq!(mgr.reserved_bytes(), 90 * 1024 * 1024);
    // Dropping C:'s reader frees its 50 MiB so a 55 MiB reservation succeeds while D: stays pinned.
    drop(c_arc);
    let reserved = mgr
        .reserve_with_eviction(55 * 1024 * 1024, &cancel)
        .expect("evicting unpinned stale C: frees enough room for 55 MiB");
    assert_eq!(mgr.reserved_bytes(), 95 * 1024 * 1024);
    drop(reserved);
    drop(d_arc);
    // D: remains cached (40 MiB) until evicted; a subsequent 80 MiB reservation evicts idle D:.
    assert_eq!(mgr.reserved_bytes(), 40 * 1024 * 1024);
    let big = mgr
        .reserve_with_eviction(80 * 1024 * 1024, &cancel)
        .expect("now-idle D: is evicted to make room for 80 MiB");
    assert_eq!(mgr.cached_bytes(), 0);
    drop(big);
    assert_eq!(mgr.reserved_bytes(), 0);
}

#[test]
fn v8_usn_growth_reconciles_memory_ledger_and_probe_timeout_preserves_ready_volumes() {
    use crate::diagnostics::volume_index::ProcessMemoryLedger;
    use std::io::Read;

    // 1. P1-4: USN replay growth synchronizes MemoryReservation and rolls back cleanly on OOM.
    let ledger = ProcessMemoryLedger::new_isolated(64 * 1024, 64 * 1024);
    let base = make_test_volume_index("C:", None);
    let mut cloned = base
        .clone_with_reservation_fn(|n| ledger.try_reserve(n))
        .expect("base clone fits in 64 KiB ledger");
    assert_eq!(ledger.reserved_bytes(), cloned.estimated_bytes);
    let many_events: Vec<UsnEventV2> = (0..64u64)
        .map(|i| UsnEventV2 {
            file_ref: ((1u64) << 48) | (1000 + i),
            parent_ref: cloned.root_ref,
            usn: 200 + i as i64,
            reason: USN_REASON_FILE_CREATE,
            file_attributes: 0x20,
            name_utf8: format!("generated_file_with_long_name_{i:04}.log"),
            name_lossless: true,
        })
        .collect();
    cloned
        .apply_usn_events_with_reserve(&many_events, 500, |n| ledger.try_reserve(n))
        .expect("USN growth fits in 64 KiB ledger");
    assert_eq!(
        ledger.reserved_bytes(),
        cloned.estimated_bytes,
        "ledger reservation must match grown estimated_bytes"
    );
    drop(cloned);
    assert_eq!(ledger.reserved_bytes(), 0);

    // Tight ledger: USN growth fails and dropping rolled-back clone frees 100% of bytes.
    let tight =
        ProcessMemoryLedger::new_isolated(base.estimated_bytes + 64, base.estimated_bytes + 64);
    let mut tight_clone = base
        .clone_with_reservation_fn(|n| tight.try_reserve(n))
        .expect("initial clone fits");
    assert!(
        tight_clone
            .apply_usn_events_with_reserve(&many_events, 500, |n| tight.try_reserve(n))
            .is_err(),
        "USN growth exceeding tight ledger must fail"
    );
    drop(tight_clone);
    assert_eq!(tight.reserved_bytes(), 0);

    // 2. P1-9: Volume enumeration preserves already-ready volumes when a later volume hangs.
    let cancel = CancellationToken::new();
    let enum_res = win_volumes::enumerate_volumes_with_probe(
        vec!['C', 'D', 'E'],
        Instant::now() + Duration::from_millis(80),
        &cancel,
        |letter, _dl, _c| {
            if letter == 'C' {
                Ok(win_volumes::ProbeDecision::Ready(
                    win_volumes::ReadyVolume {
                        display_root: "C:/".into(),
                        native_root: PathBuf::from(r"C:\"),
                        drive: "C:".into(),
                        filesystem: "NTFS".into(),
                        volume_guid: None,
                        is_subst: false,
                        serial: 1,
                    },
                ))
            } else {
                std::thread::sleep(Duration::from_millis(300));
                Ok(win_volumes::ProbeDecision::Skipped {
                    root: format!("{letter}:/"),
                    reason: "unavailable".into(),
                    non_policy_failure: true,
                })
            }
        },
    )
    .expect("ready C:/ must be preserved despite D:/ hanging");
    assert_eq!(enum_res.ready.len(), 1);
    assert_eq!(enum_res.ready[0].display_root, "C:/");
    assert!(enum_res.had_non_policy_failure);
    let skipped_names: Vec<&str> = enum_res.skipped.iter().map(|s| s.root.as_str()).collect();
    assert_eq!(skipped_names, vec!["D:/", "E:/"]);
    assert!(enum_res.skipped.iter().all(|s| s.reason == "probe_timeout"));

    // 3. P1-8: ffgrep streaming reader checks deadline after EOF read so slow EOF does not masquerade as complete.
    struct SlowEofReader;
    impl Read for SlowEofReader {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            std::thread::sleep(Duration::from_millis(50));
            Ok(0)
        }
    }
    let remaining = AtomicUsize::new(1024 * 1024);
    let matcher = grep_engine::ContentMatcher::build("needle", true, false).unwrap();
    let stream_out = grep_engine::scan_reader_streaming(
        SlowEofReader,
        "slow.log",
        1024,
        &remaining,
        &matcher,
        0,
        10,
        Instant::now() + Duration::from_millis(15),
        &cancel,
        1024,
    )
    .unwrap();
    assert!(
        stream_out.timed_out,
        "slow read returning EOF past deadline must still mark timed_out"
    );
}

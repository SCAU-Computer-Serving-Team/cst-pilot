#[path = "builtin_tools/session_executor.rs"]
mod session_executor;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use pi_agent::{AgentTool, AgentToolResult};
use pi_ai::UserContentBlock;
use pi_runtime::tools::*;
use serde_json::{Value, json};

fn fixture() -> PathBuf {
    let p = PathBuf::from("E:/tmp/2026-11-04/argon-phase4-development")
        .join(format!("tools-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

async fn call(tool: AgentTool, input: Value) -> Result<AgentToolResult, String> {
    (tool.execute)("test".into(), input, None, None).await
}

fn text(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| match c {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn fuzzy_edit_uses_nfkc_and_preserves_untouched_lines() {
    let original = "  untouched  \nﬁle ① K e\u{301}\n\tkeep  \n";
    let result = apply_edits_to_normalized_content(
        original,
        &[Edit {
            old_text: "file 1 K é".into(),
            new_text: "replacement".into(),
        }],
        "mock",
    )
    .unwrap();
    assert_eq!(result.new_content, "  untouched  \nreplacement\n\tkeep  \n");
    assert!(
        apply_edits_to_normalized_content(
            "aaaa",
            &[Edit {
                old_text: "aaa".into(),
                new_text: "x".into(),
            }],
            "mock"
        )
        .is_err(),
        "overlapping occurrences are not unique"
    );
    assert!(
        apply_edits_to_normalized_content(
            " \n",
            &[Edit {
                old_text: "\t".into(),
                new_text: "x".into(),
            }],
            "mock"
        )
        .is_err(),
        "normalization must not create an empty match"
    );
}

#[test]
fn unified_patch_handles_empty_files_no_final_newline_and_disjoint_hunks() {
    assert_eq!(generate_unified_patch("x", "same\n", "same\n"), "");
    let added = generate_unified_patch("x", "", "new");
    assert!(
        added.contains("@@ -0,0 +1 @@") || added.contains("@@ -0,0 +1,1 @@"),
        "{added}"
    );
    assert!(added.contains("\\ No newline at end of file"), "{added}");
    let old = (0..40).map(|i| format!("line{i}\n")).collect::<String>();
    let new = old
        .replace("line1\n", "first\n")
        .replace("line38\n", "last\n");
    let patch = generate_unified_patch("x", &old, &new);
    assert_eq!(
        patch.lines().filter(|l| l.starts_with("@@ ")).count(),
        2,
        "{patch}"
    );
    assert!(!patch.contains("-line20"));
    assert!(
        !generate_diff_string(&old, &new)
            .diff
            .contains("-   21 line20")
    );
}

#[test]
fn accumulator_snapshot_does_not_close_active_spool_and_structured_output_is_bounded() {
    let dir = fixture();
    let mut acc = OutputAccumulator::new(OutputAccumulatorOptions {
        max_bytes: Some(8),
        temp_dir: Some(dir.clone()),
        ..Default::default()
    });
    acc.append(b"123456789");
    let path = acc.snapshot(true).full_output_path.unwrap();
    acc.append(b"ABCDEFGHI");
    acc.finish();
    assert_eq!(std::fs::read(path).unwrap(), b"123456789ABCDEFGHI");
    assert!(acc.read_full_output(12).content.len() <= 12);
    let mut tiny = OutputAccumulator::new(OutputAccumulatorOptions::default());
    tiny.append(b"123456789");
    tiny.finish();
    assert!(tiny.read_full_output(4).content.len() <= 4);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn accumulator_decodes_split_utf8_and_reports_spool_failure_honestly() {
    let dir = fixture();
    let blocked = dir.join("file");
    std::fs::write(&blocked, "x").unwrap();
    let mut acc = OutputAccumulator::new(OutputAccumulatorOptions {
        max_bytes: Some(8),
        temp_dir: Some(blocked),
        ..Default::default()
    });
    for chunk in "你好世界你好".as_bytes().chunks(1) {
        acc.append(chunk);
    }
    acc.finish();
    let snap = acc.snapshot(true);
    assert!(!snap.content.contains('\u{fffd}'));
    assert!(snap.content.len() <= 8);
    assert!(snap.full_output_path.is_none());
    assert!(snap.temp_file_error.is_some());
    assert!(snap.truncation.truncated);
    let _ = std::fs::remove_dir_all(dir);
}

struct BlockingWrite {
    state: Arc<(Mutex<(usize, bool)>, Condvar)>,
    entered: tokio::sync::mpsc::UnboundedSender<usize>,
}
impl WriteOperations for BlockingWrite {
    fn mkdir(&self, _: &Path) -> Result<(), String> {
        Ok(())
    }
    fn write_file(&self, _: &Path, _: &str) -> Result<(), String> {
        let (lock, cv) = &*self.state;
        let mut state = lock.lock().unwrap();
        state.0 += 1;
        let n = state.0;
        self.entered.send(n).unwrap();
        while n == 1 && !state.1 {
            state = cv.wait(state).unwrap();
        }
        Ok(())
    }
}

#[tokio::test]
async fn dropped_write_caller_keeps_file_permit_until_actual_disk_operation_finishes() {
    let dir = fixture();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let state = Arc::new((Mutex::new((0, false)), Condvar::new()));
    let tool = create_write_tool(
        dir.to_string_lossy(),
        Some(WriteToolOptions {
            operations: Some(Arc::new(BlockingWrite {
                state: state.clone(),
                entered: tx,
            })),
        }),
    );
    let first = tokio::spawn(call(
        tool.clone(),
        json!({"path":"a/new/file", "content":"first"}),
    ));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap(),
        Some(1)
    );
    first.abort();
    let second = tokio::spawn(call(
        tool,
        json!({"path":"a/new/./file", "content":"second"}),
    ));
    // The blocking operation is still inside write_file after its caller is gone.
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(rx.try_recv().is_err());
    {
        let (lock, cv) = &*state;
        lock.lock().unwrap().1 = true;
        cv.notify_all();
    }
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap(),
        Some(2)
    );
    second.await.unwrap().unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn edit_is_atomic_on_error_and_preserves_bom_crlf_and_argument_forms() {
    let dir = fixture();
    let path = dir.join("x");
    std::fs::write(&path, "\u{feff}one\r\ntwo\r\nkeep  \r\n").unwrap();
    let tool = create_edit_tool(dir.to_string_lossy(), None);
    let input = prepare_edit_arguments(
        json!({"path":"x", "edits":"{\"oldText\":\"two\",\"newText\":\"changed\"}"}),
    );
    call(tool.clone(), input).await.unwrap();
    let expected = "\u{feff}one\r\nchanged\r\nkeep  \r\n";
    assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    assert!(call(tool, json!({"path":"x","edits":[{"oldText":"one","newText":"OK"},{"oldText":"missing","newText":"bad"}]})).await.is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    std::fs::write(&path, "one two\n").unwrap();
    assert!(call(create_edit_tool(dir.to_string_lossy(), None), json!({"path":"x","edits":[{"oldText":"one","newText":"first"},{"oldText":"two","newText":"second"}]})).await.is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "one two\n");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn native_find_uses_glob_sets_ignore_rules_and_directory_suffixes() {
    let dir = fixture();
    std::fs::create_dir_all(dir.join("src/sub")).unwrap();
    for p in ["src/a.rs", "src/b.ts", "src/sub/c.rs", "ignored.rs"] {
        std::fs::write(dir.join(p), "x").unwrap();
    }
    std::fs::write(dir.join(".gitignore"), "ignored.rs\n").unwrap();
    let output = text(
        &call(
            create_find_tool(
                dir.to_string_lossy(),
                Some(FindToolOptions {
                    backend: SearchBackend::Native,
                    ..Default::default()
                }),
            ),
            json!({"pattern":"**/*.{rs,ts}"}),
        )
        .await
        .unwrap(),
    );
    assert!(output.contains("src/a.rs"), "{output}");
    assert!(output.contains("src/b.ts"));
    assert!(output.contains("src/sub/c.rs"));
    assert!(!output.contains("ignored"));
    let _ = std::fs::remove_dir_all(dir);
}

struct GeneratedRead;
struct GeneratedReader {
    remaining: usize,
    chunk: [u8; 8192],
}
impl std::io::Read for GeneratedReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let n = self.remaining.min(out.len()).min(self.chunk.len());
        out[..n].copy_from_slice(&self.chunk[..n]);
        self.remaining -= n;
        Ok(n)
    }
}
impl std::io::BufRead for GeneratedReader {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        Ok(&self.chunk[..self.remaining.min(self.chunk.len())])
    }
    fn consume(&mut self, n: usize) {
        self.remaining = self.remaining.saturating_sub(n);
    }
}
impl ReadOperations for GeneratedRead {
    fn access(&self, _: &Path) -> Result<(), String> {
        Ok(())
    }
    fn detect_image_mime_type(&self, _: &Path) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn read_bytes(&self, _: &Path) -> Result<Vec<u8>, String> {
        panic!("text must use bounded streaming reader")
    }
    fn open_reader(&self, _: &Path) -> Result<Box<dyn std::io::BufRead + Send>, String> {
        Ok(Box::new(GeneratedReader {
            remaining: 64 * 1024 * 1024,
            chunk: [b'x'; 8192],
        }))
    }
}

#[tokio::test]
async fn read_streams_64mib_single_line_and_supports_pagination_and_five_images() {
    let dir = fixture();
    let huge = call(
        create_read_tool(
            dir.to_string_lossy(),
            Some(ReadToolOptions {
                operations: Some(Arc::new(GeneratedRead)),
                ..Default::default()
            }),
        ),
        json!({"path":"huge"}),
    )
    .await
    .unwrap();
    assert!(text(&huge).contains("Line 1"));
    assert!(text(&huge).contains("exceeds"));
    assert_eq!(huge.details["truncation"]["totalBytes"], 64 * 1024 * 1024);
    std::fs::write(dir.join("lines"), "one\ntwo\nthree\n").unwrap();
    let tool = create_read_tool(dir.to_string_lossy(), None);
    let page = text(
        &call(tool.clone(), json!({"path":"lines","offset":2,"limit":1}))
            .await
            .unwrap(),
    );
    assert!(page.starts_with("two\n"));
    assert!(page.contains("offset=3"));
    assert!(
        call(tool.clone(), json!({"path":"lines","offset":0}))
            .await
            .is_err()
    );
    for ext in ["png", "jpg", "gif", "webp", "bmp"] {
        std::fs::write(dir.join(format!("image.{ext}")), [1, 2, 3]).unwrap();
        let result = call(tool.clone(), json!({"path":format!("image.{ext}")}))
            .await
            .unwrap();
        assert!(
            result
                .content
                .iter()
                .any(|c| matches!(c, UserContentBlock::Image(_)))
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn ls_hidden_sorting_directory_suffix_and_limit() {
    let dir = fixture();
    std::fs::create_dir(dir.join("z-directory")).unwrap();
    for name in [".hidden", "Z.txt", "a.txt", "B.txt"] {
        std::fs::write(dir.join(name), "").unwrap();
    }
    let result = text(
        &call(create_ls_tool(dir.to_string_lossy(), None), json!({}))
            .await
            .unwrap(),
    );
    assert_eq!(result, ".hidden\na.txt\nB.txt\nz-directory/\nZ.txt");
    let result = call(
        create_ls_tool(dir.to_string_lossy(), None),
        json!({"limit":2}),
    )
    .await
    .unwrap();
    assert_eq!(result.details["entryLimitReached"], 2);
    assert!(text(&result).starts_with(".hidden\na.txt"));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn long_line_grep_matches_past_the_retained_prefix_and_auto_falls_back() {
    let dir = fixture();
    std::fs::write(
        dir.join("long.txt"),
        format!("{}TAIL-needle\r\nnext\n", "a".repeat(2 * 1024 * 1024)),
    )
    .unwrap();
    for backend in [SearchBackend::Native, SearchBackend::Auto] {
        let result = call(
            create_grep_tool(
                dir.to_string_lossy(),
                Some(GrepToolOptions {
                    backend,
                    ..Default::default()
                }),
            ),
            json!({"pattern":"tail-NEEDLE$", "ignoreCase":true}),
        )
        .await
        .unwrap();
        assert!(
            text(&result).contains("long.txt:1:"),
            "{backend:?}: {}",
            text(&result)
        );
        assert_eq!(result.details["linesTruncated"], true);
        assert!(text(&result).len() < 1024);
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn mutation_keys_resolve_uncreated_components_case_aliases_and_junction_parent() {
    let dir = fixture();
    std::fs::create_dir_all(dir.join("real/inner")).unwrap();
    let a = resolve_mutation_queue_key(&dir.join("real/inner/missing/../new/file"));
    let b = resolve_mutation_queue_key(&dir.join("real/inner/new/file"));
    assert_eq!(a, b);
    #[cfg(windows)]
    {
        assert_eq!(
            b,
            resolve_mutation_queue_key(&PathBuf::from(
                dir.join("real/inner/new/file")
                    .to_string_lossy()
                    .to_uppercase()
                    .replace('\\', "/")
            ))
        );
        let alias = dir.join("alias");
        let target = dir.join("real/inner");
        let status = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(alias.to_string_lossy().replace('/', "\\"))
            .arg(target.to_string_lossy().replace('/', "\\"))
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(resolve_mutation_queue_key(&alias.join("new/file")), b);
        // Win32 normalizes '..' before following a junction. Compare with actual I/O.
        std::fs::write(alias.join("../file"), "x").unwrap();
        assert!(dir.join("file").exists());
        assert_eq!(
            resolve_mutation_queue_key(&alias.join("../file")),
            resolve_mutation_queue_key(&dir.join("file"))
        );
        std::fs::remove_dir(alias).unwrap();
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn external_find_and_grep_match_native_contracts() {
    if ["rg", "fd"].iter().any(|exe| {
        std::process::Command::new(exe)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .status()
            .is_err()
    }) {
        return; // The optional executables are absent; native coverage remains unconditional.
    }
    let dir = fixture();
    std::fs::create_dir_all(dir.join("src/sub")).unwrap();
    std::fs::write(dir.join(".gitignore"), "ignored.rs\n").unwrap();
    std::fs::write(dir.join("ignored.rs"), "needle").unwrap();
    std::fs::write(dir.join("src/a.rs"), "before\nNeedle\nafter\n").unwrap();
    std::fs::write(dir.join("src/sub/b.ts"), "none\n").unwrap();
    let mut outputs = Vec::new();
    for backend in [SearchBackend::Native, SearchBackend::External] {
        let result = call(
            create_find_tool(
                dir.to_string_lossy(),
                Some(FindToolOptions {
                    backend,
                    ..Default::default()
                }),
            ),
            json!({"pattern":"**/*.{rs,ts}"}),
        )
        .await
        .unwrap();
        let mut paths = text(&result).lines().map(str::to_owned).collect::<Vec<_>>();
        paths.sort();
        outputs.push(paths);
    }
    assert_eq!(outputs[0], outputs[1]);
    let mut outputs = Vec::new();
    for backend in [SearchBackend::Native, SearchBackend::External] {
        let result = call(
            create_grep_tool(
                dir.to_string_lossy(),
                Some(GrepToolOptions {
                    backend,
                    ..Default::default()
                }),
            ),
            json!({"pattern":"needle","ignoreCase":true,"glob":"*.rs","context":1,"limit":1}),
        )
        .await
        .unwrap();
        outputs.push(text(&result));
    }
    assert_eq!(outputs[0], outputs[1]);
    let _ = std::fs::remove_dir_all(dir);
}

struct RemoteSearch;
impl GrepOperations for RemoteSearch {
    fn is_directory(&self, _: &Path) -> Result<bool, String> {
        Ok(true)
    }
    fn read_file(&self, _: &Path) -> Result<String, String> {
        Ok("first\nneedle\nlast\n".into())
    }
    fn visit_files(
        &self,
        root: &Path,
        _: &tokio_util::sync::CancellationToken,
        visitor: &mut dyn FnMut(&Path) -> Result<bool, String>,
    ) -> Result<(), String> {
        visitor(&root.join("remote.rs"))?;
        Ok(())
    }
}
impl FindOperations for RemoteSearch {
    fn exists(&self, _: &Path) -> bool {
        true
    }
    fn glob(
        &self,
        _: &str,
        root: &Path,
        _: usize,
        _: &tokio_util::sync::CancellationToken,
    ) -> Result<Vec<String>, String> {
        Ok(vec![root.join("remote.rs").to_string_lossy().into_owned()])
    }
}

#[tokio::test]
async fn custom_find_and_grep_operations_do_not_require_local_directories() {
    let dir = fixture().join("not-created");
    let result = call(
        create_find_tool(
            dir.to_string_lossy(),
            Some(FindToolOptions {
                operations: Some(Arc::new(RemoteSearch)),
                ..Default::default()
            }),
        ),
        json!({"pattern":"*.rs"}),
    )
    .await
    .unwrap();
    assert_eq!(text(&result), "remote.rs");
    let result = call(
        create_grep_tool(
            dir.to_string_lossy(),
            Some(GrepToolOptions {
                operations: Some(Arc::new(RemoteSearch)),
                ..Default::default()
            }),
        ),
        json!({"pattern":"needle", "context":1}),
    )
    .await
    .unwrap();
    assert_eq!(
        text(&result),
        "remote.rs-1- first\nremote.rs:2: needle\nremote.rs-3- last"
    );
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

#[tokio::test]
async fn pre_cancelled_tools_do_not_start_work_and_default_bash_runs_locally() {
    let dir = fixture();
    let tools = create_all_tools(dir.to_string_lossy(), None);
    assert_eq!(tools.len(), 8);
    assert_eq!(
        create_coding_tools(dir.to_string_lossy(), None)
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["read", "bash", "edit", "write"]
    );
    for (name, input) in [
        ("read", json!({"path":"missing"})),
        ("write", json!({"path":"missing","content":"bad"})),
        ("find", json!({"pattern":"*"})),
        ("grep", json!({"pattern":"needle"})),
        ("ls", json!({})),
        ("bash", json!({"command":"echo should-not-start"})),
        (
            "powershell",
            json!({"command":"Write-Output should-not-start"}),
        ),
        (
            "edit",
            json!({"path":"missing","edits":[{"oldText":"old","newText":"new"}]}),
        ),
    ] {
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        assert!(
            (tools[name].execute)("cancel".into(), input, Some(cancel), None)
                .await
                .is_err(),
            "{name}"
        );
    }
    assert!(!dir.join("missing").exists());
    let result = tokio::time::timeout(
        Duration::from_secs(8),
        call(tools["bash"].clone(), json!({"command":"echo bash-ok"})),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(text(&result).contains("bash-ok"));
    let _ = std::fs::remove_dir_all(dir);
}

struct BurstShell;
impl BashOperations for BurstShell {
    fn exec<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        options: BashExecOptions,
    ) -> pi_agent::BoxFuture<'a, Result<Option<i32>, String>> {
        Box::pin(async move {
            (options.on_data)(&vec![b'x'; 2 * 1024 * 1024]);
            (options.on_data)(b"TAIL");
            Ok(Some(0))
        })
    }
}

#[tokio::test]
async fn shell_structured_output_spool_bounds_and_failure_honesty() {
    let dir = fixture();
    let tool = create_bash_tool(
        dir.to_string_lossy(),
        Some(BashToolOptions {
            operations: Some(Arc::new(BurstShell)),
            temp_dir: Some(dir.clone()),
            ..Default::default()
        }),
    );
    assert!(
        call(tool.clone(), json!({"command":"mock", "timeout":0}))
            .await
            .is_err()
    );
    assert!(resolve_timeout_ms(Some(f64::INFINITY)).is_err());
    assert!(resolve_timeout_ms(Some(MAX_TIMEOUT_SECONDS + 1.0)).is_err());
    assert_eq!(resolve_timeout_ms(Some(0.000001)).unwrap(), Some(1));
    let result = call(tool, json!({"command":"mock"})).await.unwrap();
    let output = result.structured_content.as_ref().unwrap();
    assert!(output["output"].as_str().unwrap().len() <= STRUCTURED_OUTPUT_MAX_BYTES);
    assert!(output["truncated"].as_bool().unwrap());
    let path = output["full_output_path"].as_str().unwrap();
    assert_eq!(std::fs::metadata(path).unwrap().len(), 2 * 1024 * 1024 + 4);
    assert!(text(&result).contains("TAIL"));
    let blocked = dir.join("file");
    std::fs::write(&blocked, "x").unwrap();
    let result = call(
        create_bash_tool(
            dir.to_string_lossy(),
            Some(BashToolOptions {
                operations: Some(Arc::new(BurstShell)),
                temp_dir: Some(blocked),
                ..Default::default()
            }),
        ),
        json!({"command":"mock"}),
    )
    .await
    .unwrap();
    assert!(text(&result).contains("could not be saved"));
    assert!(
        result
            .structured_content
            .unwrap()
            .get("full_output_path")
            .is_none()
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(windows)]
fn process_running(pid: u32) -> bool {
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
        fn GetExitCodeProcess(handle: *mut std::ffi::c_void, code: *mut u32) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    }
    // SAFETY: query-only handle, closed on every path.
    unsafe {
        let h = OpenProcess(0x1000, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code = 0;
        let running = GetExitCodeProcess(h, &mut code) != 0 && code == 259;
        CloseHandle(h);
        running
    }
}

#[cfg(windows)]
#[tokio::test]
async fn powershell_timeout_reaps_the_running_process() {
    let dir = fixture();
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(8),
        call(
            create_powershell_tool(dir.to_string_lossy(), None),
            json!({
                "command":"Start-Sleep -Seconds 30", "timeout":0.2
            }),
        ),
    )
    .await
    .unwrap();
    assert!(result.unwrap_err().contains("timed out"));
    assert!(started.elapsed() < Duration::from_secs(5));
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(windows)]
#[tokio::test]
async fn powershell_parent_exit_kills_pipe_holding_descendant_and_drains() {
    let dir = fixture();
    let pid_file = dir.join("pid.txt");
    let command = format!(
        "$p = Start-Process powershell.exe -ArgumentList '-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 30' -NoNewWindow -PassThru; [IO.File]::WriteAllText('{}', [string]$p.Id); Write-Output 'parent-done'",
        pid_file.display()
    );
    let result = tokio::time::timeout(
        Duration::from_secs(8),
        call(
            create_powershell_tool(dir.to_string_lossy(), None),
            json!({"command":command}),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(text(&result).contains("parent-done"));
    let pid = std::fs::read_to_string(pid_file)
        .unwrap()
        .parse::<u32>()
        .unwrap();
    assert!(
        !process_running(pid),
        "descendant must be reaped before return"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(windows)]
#[tokio::test]
async fn dropping_local_shell_future_cancels_the_actual_process() {
    let dir = fixture();
    let ops = create_local_powershell_operations();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let cwd = dir.to_string_lossy().into_owned();
    let task = tokio::spawn(async move {
        ops.exec(
            "[Console]::WriteLine($PID); Start-Sleep -Seconds 30",
            &cwd,
            BashExecOptions {
                on_data: Arc::new(move |data| {
                    let _ = tx.send(data.to_vec());
                }),
                cancel: tokio_util::sync::CancellationToken::new(),
                timeout_secs: None,
                env: std::env::vars().collect(),
            },
        )
        .await
    });
    let mut line = Vec::new();
    let pid = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            line.extend(rx.recv().await.unwrap());
            if line.contains(&b'\n') {
                break String::from_utf8_lossy(&line)
                    .trim()
                    .parse::<u32>()
                    .unwrap();
            }
        }
    })
    .await
    .unwrap();
    task.abort();
    let _ = task.await;
    let gone = tokio::time::timeout(Duration::from_secs(3), async {
        while process_running(pid) {
            tokio::task::yield_now().await;
        }
    })
    .await;
    if gone.is_err() {
        pi_runtime::kill_process_tree_sync(pid);
    }
    assert!(
        gone.is_ok(),
        "dropping the waiter must cancel the process worker"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn grep_limit_context_and_output_byte_bound_apply_during_scan() {
    let dir = fixture();
    std::fs::write(dir.join("matches.txt"), "match\n".repeat(100)).unwrap();
    let result = call(
        create_grep_tool(
            dir.to_string_lossy(),
            Some(GrepToolOptions {
                backend: SearchBackend::Native,
                ..Default::default()
            }),
        ),
        json!({"pattern":"match", "limit":1,"context":2}),
    )
    .await
    .unwrap();
    let output = text(&result);
    assert_eq!(
        output
            .lines()
            .filter(|l| l.starts_with("matches.txt:"))
            .count(),
        1,
        "{output}"
    );
    std::fs::write(
        dir.join("matches.txt"),
        format!(
            "{}\nneedle\n{}",
            "line\n".repeat(20000),
            "tail\n".repeat(20000)
        ),
    )
    .unwrap();
    let result = call(
        create_grep_tool(
            dir.to_string_lossy(),
            Some(GrepToolOptions {
                backend: SearchBackend::Native,
                ..Default::default()
            }),
        ),
        json!({"pattern":"needle", "context":10000}),
    )
    .await
    .unwrap();
    assert!(text(&result).len() < DEFAULT_MAX_BYTES + 1024);
    assert!(
        !result.details.is_null(),
        "byte truncation should be reported"
    );
    let _ = std::fs::remove_dir_all(dir);
}

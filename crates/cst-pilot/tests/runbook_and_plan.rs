use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use cst_pilot::runbook::{
    RunbookStore,
    model::{DeliverInput, PlanInput, RunbookInput},
    sequence_store::DirectoryLock,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("cst-step3-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        Self(root.canonicalize().unwrap())
    }
    fn store(&self) -> RunbookStore {
        RunbookStore::new(&self.0).unwrap()
    }
    fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.0.join(relative)
    }
    fn put(&self, relative: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> PathBuf {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }
    fn day(&self) -> PathBuf {
        self.path("artifacts/2026-11-08/runbook")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn input() -> Value {
    json!({"title":"清理临时文件","level":"safe","date":"2026-11-08","items":[
        {"summary":"清理指定测试目录的缓存，已打开的文件可能无法删除。","command":"  echo manual-only  ","shell":"powershell"}
    ]})
}
fn decode<T: DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap()
}
fn revision(plan: &Value) -> Value {
    let mut input = input();
    input["runbookId"] = plan["runbook"]["runbookId"].clone();
    input["baseVersion"] = plan["runbook"]["version"].clone();
    input
}
fn delivery(plan: &Value) -> DeliverInput {
    decode(json!({"runbookId":plan["runbook"]["runbookId"],"version":plan["runbook"]["version"]}))
}
fn file(result: &Value) -> PathBuf {
    PathBuf::from(result["runbook"]["file"].as_str().unwrap())
}
fn plan_path(fixture: &Fixture, plan: &Value, version: u64) -> PathBuf {
    fixture.path(format!(
        "agent/home/command-plans/{}/v{version}.json",
        plan["runbook"]["runbookId"].as_str().unwrap()
    ))
}
fn entries(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

#[tokio::test]
async fn plan_revision_latest_delivery_and_old_exports_are_immutable() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let plan = store
        .plan(decode(input()), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(plan["runbook"]["action"], "planned");
    assert_eq!(plan["runbook"]["version"], 1);
    assert_eq!(plan["runbook"]["items"], 1);
    assert_eq!(plan["runbook"]["commands"][0]["admin"], false);
    assert_eq!(
        plan["runbook"]["commands"][0]["command"],
        "echo manual-only"
    );
    let stored: Value =
        serde_json::from_slice(&fs::read(plan_path(&fixture, &plan, 1)).unwrap()).unwrap();
    assert_eq!(stored["commands"], plan["runbook"]["commands"]);
    assert!(stored.get("items").is_none());
    let exported = store
        .deliver(delivery(&plan), CancellationToken::new())
        .await
        .unwrap();
    let original = fs::read(file(&exported)).unwrap();
    let mut change = revision(&plan);
    change["items"][0]["command"] = json!("echo revised");
    let revised = fixture
        .store()
        .plan(decode(change), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        revised["runbook"]["runbookId"],
        plan["runbook"]["runbookId"]
    );
    assert_eq!(revised["runbook"]["version"], 2);
    assert!(
        store
            .deliver(delivery(&plan), CancellationToken::new())
            .await
            .unwrap_err()
            .contains("新版本 2")
    );
    let second = store
        .deliver(delivery(&revised), CancellationToken::new())
        .await
        .unwrap();
    assert_ne!(file(&exported), file(&second));
    assert_eq!(fs::read(file(&exported)).unwrap(), original);
    assert_eq!(entries(file(&second).parent().unwrap()).len(), 2);
}

#[tokio::test]
async fn parallel_revisions_use_one_shared_compare_and_swap_lock() {
    let fixture = Fixture::new();
    let a = fixture.store();
    let b = fixture.store();
    let plan = a
        .plan(decode(input()), CancellationToken::new())
        .await
        .unwrap();
    let (x, y) = tokio::join!(
        a.plan(decode(revision(&plan)), CancellationToken::new()),
        b.plan(decode(revision(&plan)), CancellationToken::new())
    );
    assert_ne!(x.is_ok(), y.is_ok());
    let error = if let Err(e) = x { e } else { y.unwrap_err() };
    assert!(error.contains("新版本 2"));
    assert_eq!(
        entries(plan_path(&fixture, &plan, 1).parent().unwrap()).len(),
        2
    );
}

#[tokio::test]
async fn repeated_delivery_uses_saved_time_and_preserves_manual_edits() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let plan = store
        .plan(decode(input()), CancellationToken::new())
        .await
        .unwrap();
    let path = plan_path(&fixture, &plan, 1);
    let mut saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    saved["timeText"] = json!("2024-02-29 23:59");
    saved["createdAt"] = json!("2024-03-01T07:59:00.000Z");
    fs::write(path, serde_json::to_vec(&saved).unwrap()).unwrap();
    let first = store
        .deliver(delivery(&plan), CancellationToken::new())
        .await
        .unwrap();
    let path = file(&first);
    let bytes = fs::read(&path).unwrap();
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let second = fixture
        .store()
        .deliver(delivery(&plan), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
    assert!(
        String::from_utf8(bytes)
            .unwrap()
            .contains("2024-02-29 23:59")
    );
    fs::write(&path, "队员现场补充").unwrap();
    assert!(
        store
            .deliver(delivery(&plan), CancellationToken::new())
            .await
            .unwrap_err()
            .contains("被修改")
    );
    assert_eq!(fs::read_to_string(path).unwrap(), "队员现场补充");
}

#[tokio::test]
async fn legacy_typescript_plan_is_read_and_revised_into_the_modern_directory() {
    let fixture = Fixture::new();
    let id = uuid::Uuid::new_v4().to_string();
    let raw = json!({"runbookId":id,"version":7,"day":"2026-11-08","createdAt":"2026-01-01T00:00:00.000Z",
    "timeText":"2026-01-01 08:00","title":"已有方案","level":"low","commands":[
        {"summary":"保留已有配置，需手动核对。","command":"echo legacy","shell":"cmd"}
    ]});
    let legacy = fixture.put(
        format!("outbox/.plans/{id}/v7.json"),
        serde_json::to_vec(&raw).unwrap(),
    );
    let delivered = fixture
        .store()
        .deliver(
            decode(json!({"runbookId":id,"version":7})),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(delivered["runbook"]["commands"][0]["admin"], false);
    let mut change = input();
    change["runbookId"] = json!(id);
    change["baseVersion"] = json!(7);
    change.as_object_mut().unwrap().remove("date");
    let plan = fixture
        .store()
        .plan(decode(change), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(plan["runbook"]["version"], 8);
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(legacy).unwrap()).unwrap(),
        raw
    );
    assert!(plan_path(&fixture, &plan, 8).is_file());
    assert!(
        fixture
            .store()
            .deliver(
                decode(json!({"runbookId":id,"version":7})),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn corrupted_latest_plan_only_blocks_its_own_id_and_is_never_overwritten() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let plan = store
        .plan(decode(input()), CancellationToken::new())
        .await
        .unwrap();
    let path = plan_path(&fixture, &plan, 2);
    fs::write(&path, b"{partial").unwrap();
    assert!(
        store
            .plan(decode(revision(&plan)), CancellationToken::new())
            .await
            .unwrap_err()
            .contains("损坏")
    );
    assert!(
        store
            .deliver(delivery(&plan), CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(fs::read(path).unwrap(), b"{partial");
    assert!(
        store
            .plan(decode(input()), CancellationToken::new())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn invalid_stored_dates_and_identifiers_cannot_redirect_exports() {
    for (field, value) in [
        ("day", json!("../../outside")),
        ("timeText", json!("2026-02-30 08:00")),
        ("runbookId", json!("../other")),
        ("createdAt", json!("invalid")),
        ("version", json!(2)),
    ] {
        let fixture = Fixture::new();
        let store = fixture.store();
        let plan = store
            .plan(decode(input()), CancellationToken::new())
            .await
            .unwrap();
        let path = plan_path(&fixture, &plan, 1);
        let mut raw: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        raw[field] = value;
        let bytes = serde_json::to_vec(&raw).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(
            store
                .deliver(delivery(&plan), CancellationToken::new())
                .await
                .is_err(),
            "{field}"
        );
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert!(!fixture.path("artifacts").exists());
    }
}

#[tokio::test]
async fn validation_rejects_multiline_out_of_range_and_incomplete_revision_inputs() {
    let fixture = Fixture::new();
    let store = fixture.store();
    for (field, value) in [
        ("title", json!(" ")),
        ("title", json!("a".repeat(49))),
        ("items", json!([])),
        ("items", json!(vec![input()["items"][0].clone(); 21])),
        ("date", json!("2026-02-30")),
        ("date", json!("2026-1-08")),
        ("date", json!("0000-01-01")),
        ("date", json!("../../outside")),
        ("runbookId", json!("../outside")),
        ("baseVersion", json!(1)),
    ] {
        let mut bad = input();
        bad[field] = value;
        assert!(
            store
                .plan(decode(bad), CancellationToken::new())
                .await
                .is_err(),
            "{field}"
        );
    }
    for field in ["summary", "command"] {
        for text in [
            "",
            "\necho x",
            "echo x\r\necho y",
            "echo\0x",
            "echo\u{2028}x",
        ] {
            let mut bad = input();
            bad["items"][0][field] = json!(text);
            assert!(
                store
                    .plan(decode(bad), CancellationToken::new())
                    .await
                    .is_err()
            );
        }
    }
    for (field, len) in [("summary", 301), ("command", 2001)] {
        let mut bad = input();
        bad["items"][0][field] = json!("文".repeat(len));
        assert!(
            store
                .plan(decode(bad), CancellationToken::new())
                .await
                .is_err()
        );
    }
    for (field, value) in [
        ("level", json!("danger")),
        (
            "items",
            json!([{"shell":"bash","command":"echo x","summary":"说明"}]),
        ),
        (
            "items",
            json!([{"shell":"cmd","command":"echo x","summary":"说明","admin":"yes"}]),
        ),
    ] {
        let mut bad = input();
        bad[field] = value;
        assert!(serde_json::from_value::<PlanInput>(bad).is_err());
    }
    assert!(!fixture.path("agent").exists());
}

#[tokio::test]
async fn archive_date_is_fixed_and_foreign_ids_or_versions_are_rejected() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let plan = store
        .plan(decode(input()), CancellationToken::new())
        .await
        .unwrap();
    let mut bad = revision(&plan);
    bad["date"] = json!("2026-11-09");
    assert!(
        store
            .plan(decode(bad), CancellationToken::new())
            .await
            .unwrap_err()
            .contains("日期不能修改")
    );
    for id in [
        "../outside",
        "A0000000-0000-0000-0000-000000000000",
        "missing",
    ] {
        assert!(
            store
                .deliver(
                    decode(json!({"runbookId":id,"version":1})),
                    CancellationToken::new()
                )
                .await
                .is_err()
        );
    }
    for version in [0, 9_007_199_254_740_992] {
        let mut request = delivery(&plan);
        request.version = version;
        assert!(
            store
                .deliver(request, CancellationToken::new())
                .await
                .is_err()
        );
    }
    assert_eq!(
        entries(plan_path(&fixture, &plan, 1).parent().unwrap()).len(),
        1
    );
}

#[tokio::test]
async fn sequence_encoding_normalization_and_rename_preserve_one_file() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let mut source = input();
    source["title"] = json!("清/理:*?<缓存>.  ");
    source["items"][0]["admin"] = json!(true);
    let first = store
        .write(decode(source), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(first["runbook"]["sequence"], 1);
    assert_eq!(first["runbook"]["title"], "清理缓存");
    assert_eq!(first["runbook"]["commands"][0]["shell"], "PowerShell");
    let bytes = fs::read(file(&first)).unwrap();
    assert_eq!(&bytes[..3], b"\xef\xbb\xbf");
    assert_eq!(first["runbook"]["bytes"], bytes.len());
    let text = String::from_utf8(bytes).unwrap();
    assert!(!text.replace("\r\n", "").contains(['\r', '\n']));
    let lines: Vec<_> = text.split("\r\n").collect();
    assert_eq!(
        &lines[2..6],
        [
            "环境：PowerShell（管理员）",
            "说明：清理指定测试目录的缓存，已打开的文件可能无法删除。",
            "命令：",
            "echo manual-only"
        ]
    );
    let mut change = input();
    change["title"] = json!("新标题");
    change["sequence"] = json!(1);
    change["level"] = json!("confirm");
    let second = store
        .write(decode(change), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(second["runbook"]["action"], "replaced");
    assert!(!file(&first).exists());
    assert!(file(&second).is_file());
    assert_eq!(entries(&fixture.day()).len(), 1);
    assert!(
        second["runbook"]["name"]
            .as_str()
            .unwrap()
            .starts_with("01-需确认-")
    );
}

#[tokio::test]
async fn same_name_replacement_and_parallel_allocations_are_unique_and_daily() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let initial = store
        .write(decode(input()), CancellationToken::new())
        .await
        .unwrap();
    let mut change = input();
    change["sequence"] = json!(1);
    change["items"][0]["command"] = json!("echo change");
    let result = store
        .write(decode(change), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(file(&initial), file(&result));
    assert!(
        fs::read_to_string(file(&result))
            .unwrap()
            .contains("echo change")
    );
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let cloned = fixture.store();
        tasks.push(tokio::spawn(async move {
            cloned
                .write(decode(input()), CancellationToken::new())
                .await
                .unwrap()["runbook"]["sequence"]
                .as_u64()
                .unwrap()
        }));
    }
    let mut numbers = Vec::new();
    for task in tasks {
        numbers.push(task.await.unwrap());
    }
    numbers.sort();
    assert_eq!(numbers, (2..=9).collect::<Vec<_>>());
    let mut tomorrow = input();
    tomorrow["date"] = json!("2026-11-09");
    assert_eq!(
        store
            .write(decode(tomorrow), CancellationToken::new())
            .await
            .unwrap()["runbook"]["sequence"],
        1
    );
    assert_eq!(entries(&fixture.day()).len(), 9);
}

#[tokio::test]
async fn duplicate_missing_and_exhausted_slots_are_rejected_without_changes() {
    let fixture = Fixture::new();
    let store = fixture.store();
    for sequence in [0, 1000, 1] {
        let mut bad = input();
        bad["sequence"] = json!(sequence);
        assert!(
            store
                .write(decode(bad), CancellationToken::new())
                .await
                .is_err()
        );
    }
    fixture.put("artifacts/2026-11-08/runbook/01-安全-a.txt", "a");
    fixture.put("artifacts/2026-11-08/runbook/01-较安全-b.txt", "b");
    assert!(
        store
            .write(decode(input()), CancellationToken::new())
            .await
            .unwrap_err()
            .contains("重复")
    );
    fs::remove_file(fixture.day().join("01-较安全-b.txt")).unwrap();
    fixture.put("artifacts/2026-11-08/runbook/999-安全-full.txt", "full");
    assert!(
        store
            .write(decode(input()), CancellationToken::new())
            .await
            .unwrap_err()
            .contains("999")
    );
    assert_eq!(entries(&fixture.day()).len(), 2);
}

#[tokio::test]
async fn blocked_output_preserves_old_slot_and_cleans_temporary_files() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let first = store
        .write(decode(input()), CancellationToken::new())
        .await
        .unwrap();
    let original = fs::read(file(&first)).unwrap();
    let obstruction = fixture.day().join("01-安全-阻挡.txt");
    fs::create_dir(&obstruction).unwrap();
    let mut input = input();
    input["title"] = json!("阻挡");
    input["sequence"] = json!(1);
    assert!(
        store
            .write(decode(input), CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(fs::read(file(&first)).unwrap(), original);
    assert!(
        !entries(&fixture.day())
            .iter()
            .any(|name| name.ends_with(".tmp"))
    );
}

#[tokio::test]
async fn cancelled_requests_do_not_create_files_or_propagate_cancellation_to_the_caller() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(
        store
            .plan(decode(input()), cancel.clone())
            .await
            .unwrap_err()
            .contains("取消")
    );
    assert!(store.write(decode(input()), cancel).await.is_err());
    assert!(!fixture.path("agent").exists());
    assert!(!fixture.path("artifacts").exists());
    let parent = CancellationToken::new();
    store.plan(decode(input()), parent.clone()).await.unwrap();
    assert!(!parent.is_cancelled());
}

#[tokio::test]
async fn hardlinks_to_plans_and_delivered_files_are_rejected_without_touching_the_alias() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let plan = store
        .plan(decode(input()), CancellationToken::new())
        .await
        .unwrap();
    let path = plan_path(&fixture, &plan, 1);
    let alias = fixture.path("outside-plan.json");
    fs::hard_link(&path, &alias).unwrap();
    let bytes = fs::read(&alias).unwrap();
    assert!(
        store
            .deliver(delivery(&plan), CancellationToken::new())
            .await
            .unwrap_err()
            .contains("硬链接")
    );
    fs::remove_file(alias).unwrap();
    let exported = store
        .deliver(delivery(&plan), CancellationToken::new())
        .await
        .unwrap();
    let alias = fixture.path("outside.txt");
    fs::hard_link(file(&exported), &alias).unwrap();
    let text = fs::read(&alias).unwrap();
    assert!(
        store
            .deliver(delivery(&plan), CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(fs::read(&alias).unwrap(), text);
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[cfg(windows)]
#[tokio::test]
async fn cleanup_failure_rolls_back_new_name_while_the_old_file_is_open() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    let store = fixture.store();
    let first = store
        .write(decode(input()), CancellationToken::new())
        .await
        .unwrap();
    let original = fs::read(file(&first)).unwrap();
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(file(&first))
        .unwrap();
    let mut change = input();
    change["title"] = json!("新标题");
    change["sequence"] = json!(1);
    assert!(
        store
            .write(decode(change), CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(entries(&fixture.day()).len(), 1);
    assert_eq!(fs::read(file(&first)).unwrap(), original);
    drop(held);
}

#[cfg(windows)]
#[tokio::test]
async fn junctions_at_each_writable_boundary_are_rejected_before_any_outside_write() {
    for relative in [
        "agent",
        "agent/home",
        "agent/home/command-plans",
        "artifacts",
        "artifacts/2026-11-08",
        "artifacts/2026-11-08/runbook",
    ] {
        let fixture = Fixture::new();
        let outside = Fixture::new();
        let path = fixture.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // mklink creates a junction within isolated test fixtures, without elevation.
        let status = Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&path)
            .arg(&outside.0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        let result = if relative.starts_with("agent") {
            fixture
                .store()
                .plan(decode(input()), CancellationToken::new())
                .await
        } else {
            fixture
                .store()
                .write(decode(input()), CancellationToken::new())
                .await
        };
        assert!(result.is_err(), "{relative}");
        assert!(entries(&outside.0).is_empty(), "{relative}");
        fs::remove_dir(path).unwrap();
    }
}

#[test]
#[ignore = "helper process for directory-lock tests"]
fn directory_lock_child() {
    let dir = std::env::var_os("CST_RUNBOOK_LOCK_DIR").expect("isolated test directory");
    let ready = std::env::var_os("CST_RUNBOOK_LOCK_READY").expect("readiness file");
    let _guard = DirectoryLock::acquire(Path::new(&dir)).unwrap();
    fs::write(ready, b"ready").unwrap();
    std::thread::sleep(Duration::from_secs(60));
}
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn lock_child(directory: &Path, ready: &Path) -> ChildGuard {
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "directory_lock_child",
            "--nocapture",
        ])
        .env("CST_RUNBOOK_LOCK_DIR", directory)
        .env("CST_RUNBOOK_LOCK_READY", ready)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let child = ChildGuard(child);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        assert!(
            Instant::now() < deadline,
            "lock helper did not become ready"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}
#[tokio::test]
async fn independent_process_locks_cover_both_plan_and_sequence_writers_and_release_on_exit() {
    for relative in ["agent/home/command-plans", "artifacts/2026-11-08/runbook"] {
        let fixture = Fixture::new();
        let dir = fixture.path(relative);
        fs::create_dir_all(&dir).unwrap();
        let child = lock_child(&dir, &fixture.path("ready"));
        let result = if relative.starts_with("agent") {
            fixture
                .store()
                .plan(decode(input()), CancellationToken::new())
                .await
        } else {
            fixture
                .store()
                .write(decode(input()), CancellationToken::new())
                .await
        };
        assert!(result.unwrap_err().contains("另一进程"));
        assert!(entries(&dir).is_empty());
        drop(child);
        assert!(DirectoryLock::acquire(&dir).is_ok());
    }
}

#[tokio::test]
async fn public_definitions_are_schema_checked_and_never_replay_writes() {
    let fixture = Fixture::new();
    let defs = cst_pilot::runbook::definitions(&fixture.0).unwrap();
    assert_eq!(
        defs.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        ["command_plan", "runbook_deliver", "runbook"]
    );
    for definition in &defs {
        assert!(definition.prompt_snippet.is_some());
        assert!(!definition.prompt_guidelines.as_ref().unwrap().is_empty());
        assert_eq!(definition.replay_policy, pi_agent::ToolReplayPolicy::Never);
    }
    assert!(
        serde_json::from_value::<RunbookInput>(
            json!({"title":"a","level":"safe","items":[],"path":"../../outside"})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<PlanInput>(
            json!({"title":"a","level":"safe","items":[],"file":"outside"})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<DeliverInput>(
            json!({"runbookId":"x","version":1,"date":"2026-11-09"})
        )
        .is_err()
    );
}

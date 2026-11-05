use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use pi_runtime::{
    CodemodeDeclaredTool, CodemodeErrorKind, CodemodeRunOptions, CodemodeSandbox,
    CodemodeSandboxOptions,
};
use serde_json::json;

fn options() -> CodemodeRunOptions {
    CodemodeRunOptions {
        started_at: 1_700_000_000_000.0,
        seed: 42,
        tools: Vec::new(),
        globals: Vec::new(),
        timeout_ms: Some(2_000),
        cancel_token: None,
        store: BTreeMap::new(),
        on_started: None,
        on_call: Arc::new(|_| {}),
        on_idle: None,
    }
}

#[tokio::test]
async fn deterministic_dates_use_utc_for_construction_parsing_formatting_and_subclasses() {
    let sandbox = CodemodeSandbox::new(CodemodeSandboxOptions::default()).unwrap();
    let run = sandbox
        .start(
            r#"
        class CustomDate extends Date {}
        const date = new Date(2024, 0, 2, 3, 4, 5);
        const old = new Date(2000, 0, 2); old.setYear(124);
        return {
            now: Date.now(), current: +new Date(), call: Date(),
            parts: date.toISOString(), parse: Date.parse('2024-01-02T03:04:05'),
            legacy: Date.parse('January 2, 2024 03:04:05'),
            offset: date.getTimezoneOffset(), year: date.getYear(),
            changedYear: old.getFullYear(), hours: date.getHours(),
            day: date.toDateString(), time: date.toTimeString(),
            locale: date.toLocaleString(),
            hiddenNow: typeof Object.getPrototypeOf(Date).now,
            subclass: new CustomDate() instanceof CustomDate,
            constructor: new Date().constructor === Date,
            invalidOffset: String(new Date(NaN).getTimezoneOffset())
        };
    "#,
            options(),
        )
        .unwrap();
    let result = run.wait().await;
    assert!(result.ok, "{:?}", result.error);
    let value = result.value.unwrap();
    assert_eq!(value["now"], json!(1_700_000_000_000u64));
    assert_eq!(value["current"], value["now"]);
    assert_eq!(value["parts"], "2024-01-02T03:04:05.000Z");
    assert_eq!(value["parse"], json!(1_704_164_645_000u64));
    assert_eq!(value["legacy"], value["parse"]);
    assert_eq!(value["offset"], 0);
    assert_eq!(value["hours"], 3);
    assert_eq!(value["year"], 124);
    assert_eq!(value["changedYear"], 124);
    assert_eq!(value["day"], "Tue Jan 02 2024");
    assert!(
        value["time"]
            .as_str()
            .unwrap()
            .contains("03:04:05 GMT+0000")
    );
    assert!(value["locale"].as_str().unwrap().contains("3:04:05"));
    assert!(value["call"].as_str().unwrap().contains("GMT+0000"));
    assert_eq!(value["hiddenNow"], "undefined");
    assert_eq!(value["subclass"], true);
    assert_eq!(value["constructor"], true);
    assert_eq!(value["invalidOffset"], "NaN");
}

#[test]
fn deterministic_timezone_is_independent_of_process_timezone() {
    let exe = std::env::current_exe().unwrap();
    for timezone in ["UTC", "Pacific/Honolulu", "Asia/Shanghai"] {
        let output = std::process::Command::new(&exe)
            .args([
                "--exact",
                "deterministic_dates_use_utc_for_construction_parsing_formatting_and_subclasses",
                "--nocapture",
            ])
            .env("TZ", timezone)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{timezone}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[tokio::test]
async fn closing_and_dropping_sandbox_cancel_started_runs_and_pending_callbacks() {
    for close_explicitly in [true, false] {
        let sandbox = CodemodeSandbox::new(CodemodeSandboxOptions::default()).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut opts = options();
        opts.timeout_ms = None;
        opts.tools = vec![CodemodeDeclaredTool {
            name: "pending".to_owned(),
            ..Default::default()
        }];
        opts.on_call = Arc::new(move |call| {
            let _ = tx.send(call);
        });
        let run = sandbox
            .start("return await tools.pending({});", opts)
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if close_explicitly {
            sandbox.close().await;
        }
        drop(sandbox);
        let result = tokio::time::timeout(Duration::from_secs(2), run.wait())
            .await
            .unwrap();
        assert_eq!(result.error.unwrap().kind, CodemodeErrorKind::Aborted);
        assert!(run.is_finished());
    }
}

#[tokio::test]
async fn dropping_run_releases_callbacks_and_interrupts_busy_vm() {
    let sandbox = CodemodeSandbox::new(CodemodeSandboxOptions::default()).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut opts = options();
    opts.timeout_ms = None;
    opts.on_started = Some(Arc::new(move |_| {
        let _ = tx.send(());
    }));
    let run = sandbox.start("while (true) {}", opts).unwrap();
    rx.recv().await.unwrap();
    drop(run);
    assert!(
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .is_none()
    );
    sandbox.close().await;
}

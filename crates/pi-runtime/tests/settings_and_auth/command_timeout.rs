use super::*;

#[tokio::test]
async fn command_timeout_after_ten_seconds_reaps_descendants_and_redacts_output() {
    let dir = root();
    let pid_file = dir.join("child.txt");
    let command = format!(
        "!powershell.exe -NoProfile -NonInteractive -Command \"$p = Start-Process powershell.exe -ArgumentList '-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 60' -NoNewWindow -PassThru; [IO.File]::WriteAllText('{}', [string]$p.Id); Write-Output 'mock-secret'; Start-Sleep -Seconds 60\"",
        pid_file.display()
    );
    let resolver = ConfigValueResolver::new(&dir, &dir, true);
    let started = std::time::Instant::now();
    let error = tokio::time::timeout(
        Duration::from_secs(15),
        resolver.resolve_or_throw_async(
            command.clone(),
            "credential",
            None,
            ConfigValueSource::Global,
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.contains("timed out after 10s"), "{error}");
    assert!(!error.contains("mock-secret"));
    assert!(!error.contains(&command));
    assert!(started.elapsed() >= Duration::from_secs(9));
    let pid: u32 = std::fs::read_to_string(pid_file).unwrap().parse().unwrap();
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
        fn GetExitCodeProcess(handle: *mut std::ffi::c_void, code: *mut u32) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    }
    // SAFETY: query-only process handle; close before asserting.
    let running = unsafe {
        let h = OpenProcess(0x1000, 0, pid);
        if h.is_null() {
            false
        } else {
            let mut code = 0;
            let running = GetExitCodeProcess(h, &mut code) != 0 && code == 259;
            CloseHandle(h);
            running
        }
    };
    assert!(!running, "credential command descendant survived timeout");
}

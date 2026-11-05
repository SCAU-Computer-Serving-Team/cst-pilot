use super::*;
use pi_ai::faux::*;
use pi_runtime::*;
use std::collections::BTreeMap;

#[cfg(windows)]
#[tokio::test]
async fn panicking_output_callback_reaps_running_shell_and_returns_redacted_error() {
    let root = fixture();
    let pid_file = root.join("callback-pid.txt");
    let command = format!(
        "[IO.File]::WriteAllText('{}', [string]$PID); Write-Output 'ready'; Start-Sleep -Seconds 60",
        pid_file.display()
    );
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        create_local_powershell_operations().exec(
            &command,
            &root.to_string_lossy(),
            BashExecOptions {
                on_data: Arc::new(|_| panic!("mock callback failure")),
                cancel: Default::default(),
                timeout_secs: None,
                env: std::env::vars().collect(),
            },
        ),
    )
    .await
    .unwrap();
    assert_eq!(result.unwrap_err(), "Shell output callback panicked");
    let pid = std::fs::read_to_string(pid_file).unwrap().parse().unwrap();
    assert!(!super::process_running(pid));
}

type Invocation = (String, BTreeMap<String, String>);
struct CaptureShell(Arc<Mutex<Vec<Invocation>>>);
impl BashOperations for CaptureShell {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        _: &'a str,
        options: BashExecOptions,
    ) -> pi_agent::BoxFuture<'a, Result<Option<i32>, String>> {
        Box::pin(async move {
            self.0.lock().unwrap().push((command.into(), options.env));
            (options.on_data)(b"local mock output");
            Ok(Some(0))
        })
    }
}

#[tokio::test]
async fn sdk_bash_tool_and_user_bash_share_operations_policy_and_session_environment() {
    for expose in [true, false] {
        let root = fixture();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let faux = create_faux_provider();
        faux.set_responses([
            faux_assistant_message(
                vec![faux_tool_call(
                    "bash",
                    json!({"command":"tool-command"})
                        .as_object()
                        .unwrap()
                        .clone(),
                    Some("shell".into()),
                )],
                Some(pi_ai::StopReason::ToolUse),
            )
            .into(),
            faux_text_message("done").into(),
        ]);
        let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            include_builtins: Some(false),
            refresh_on_create: Some(false),
            ..Default::default()
        })
        .await;
        runtime.register_provider(faux.provider.clone());
        let result = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(root.clone()),
            agent_dir: Some(root.join("agent")),
            model_runtime: Some(runtime),
            model: Some(faux.model),
            thinking_level: Some(pi_ai::ModelThinkingLevel::Off),
            tools: Some(vec!["bash".into()]),
            tools_options: Some(ToolsOptions {
                bash: Some(BashToolOptions {
                    operations: Some(Arc::new(CaptureShell(calls.clone()))),
                    command_prefix: Some("prefix".into()),
                    expose_session_environment: Some(expose),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .unwrap();
        result.session.prompt("run tool", None).await.unwrap();
        assert_eq!(
            result
                .session
                .execute_bash("user-command", Default::default())
                .await
                .unwrap()
                .output,
            "local mock output"
        );
        let id = result.session.session_id();
        {
            let calls = calls.lock().unwrap();
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[0].0, "prefix\ntool-command");
            assert_eq!(calls[1].0, "prefix\nuser-command");
            for (_, env) in calls.iter() {
                if expose {
                    assert_eq!(env["PI_SESSION_ID"], id);
                    assert_eq!(env["PI_PROVIDER"], "faux");
                    assert_eq!(env["PI_MODEL"], "faux-1");
                    assert_eq!(env["PI_REASONING_LEVEL"], "off");
                    assert!(env.contains_key("PI_SESSION_FILE"));
                } else {
                    assert!(!env.keys().any(|k| matches!(
                        k.as_str(),
                        "PI_SESSION_ID"
                            | "PI_SESSION_FILE"
                            | "PI_PROVIDER"
                            | "PI_MODEL"
                            | "PI_REASONING_LEVEL"
                    )));
                }
            }
        }
        result.session.shutdown().await.unwrap();
    }
}

#[test]
fn powershell_config_follows_five_tier_windows_lookup_and_rejects_non_windows() {
    let mut env = BTreeMap::new();
    env.insert("ProgramFiles".into(), r"D:\Apps".into());
    env.insert("SystemRoot".into(), r"C:\Windows".into());

    // Non-Windows rejection
    let err = get_powershell_config_with(&PowerShellConfigLookup {
        platform: "linux",
        env: &env,
        find_in_path: &|_| Some("/usr/bin/pwsh".into()),
        path_exists: &|_| true,
    })
    .unwrap_err();
    assert_eq!(err, "The powershell tool is only available on Windows.");

    // Tier 1: pwsh.exe on PATH
    let cfg = get_powershell_config_with(&PowerShellConfigLookup {
        platform: "win32",
        env: &env,
        find_in_path: &|exe| {
            (exe == "pwsh.exe").then(|| r"C:\Users\me\scoop\shims\pwsh.exe".into())
        },
        path_exists: &|_| true,
    })
    .unwrap();
    assert_eq!(cfg.shell, r"C:\Users\me\scoop\shims\pwsh.exe");
    assert_eq!(cfg.args, POWERSHELL_ARGS);

    // Tier 2: %ProgramFiles%\PowerShell\7\pwsh.exe
    let cfg = get_powershell_config_with(&PowerShellConfigLookup {
        platform: "win32",
        env: &env,
        find_in_path: &|exe| {
            (exe == "powershell.exe")
                .then(|| r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe".into())
        },
        path_exists: &|p| p == r"D:\Apps\PowerShell\7\pwsh.exe",
    })
    .unwrap();
    assert_eq!(cfg.shell, r"D:\Apps\PowerShell\7\pwsh.exe");

    // Tier 3: powershell.exe on PATH
    let cfg = get_powershell_config_with(&PowerShellConfigLookup {
        platform: "win32",
        env: &env,
        find_in_path: &|exe| {
            (exe == "powershell.exe").then(|| r"E:\CustomPS\powershell.exe".into())
        },
        path_exists: &|_| false,
    })
    .unwrap();
    assert_eq!(cfg.shell, r"E:\CustomPS\powershell.exe");

    // Tier 4: %SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe
    let cfg = get_powershell_config_with(&PowerShellConfigLookup {
        platform: "win32",
        env: &env,
        find_in_path: &|_| None,
        path_exists: &|p| p == r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
    })
    .unwrap();
    assert_eq!(
        cfg.shell,
        r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
    );

    // Tier 5: Error when no executable exists
    let err = get_powershell_config_with(&PowerShellConfigLookup {
        platform: "win32",
        env: &env,
        find_in_path: &|_| None,
        path_exists: &|_| false,
    })
    .unwrap_err();
    assert_eq!(
        err,
        "No PowerShell executable found. Install PowerShell or add powershell.exe/pwsh.exe to PATH."
    );
}

#[tokio::test]
async fn sdk_powershell_tool_propagates_settings_and_session_environment() {
    for expose in [true, false] {
        let root = fixture();
        let agent_dir = root.join("agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("settings.json"),
            serde_json::to_string(&json!({
                "exposeSessionEnvironment": expose,
                "shellCommandPrefix": "$ErrorActionPreference = 'Stop'"
            }))
            .unwrap(),
        )
        .unwrap();

        let calls = Arc::new(Mutex::new(Vec::new()));
        let faux = create_faux_provider();
        faux.set_responses([
            faux_assistant_message(
                vec![faux_tool_call(
                    "powershell",
                    json!({"command": "Get-ChildItem"})
                        .as_object()
                        .unwrap()
                        .clone(),
                    Some("ps-1".into()),
                )],
                Some(pi_ai::StopReason::ToolUse),
            )
            .into(),
            faux_text_message("done").into(),
        ]);
        let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            include_builtins: Some(false),
            refresh_on_create: Some(false),
            ..Default::default()
        })
        .await;
        runtime.register_provider(faux.provider.clone());
        let result = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(root.clone()),
            agent_dir: Some(agent_dir),
            model_runtime: Some(runtime),
            model: Some(faux.model),
            thinking_level: Some(pi_ai::ModelThinkingLevel::Off),
            tools: Some(vec!["powershell".into()]),
            tools_options: Some(ToolsOptions {
                powershell: Some(PowerShellToolOptions {
                    operations: Some(Arc::new(CaptureShell(calls.clone()))),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .unwrap();
        result.session.prompt("run ps", None).await.unwrap();
        let id = result.session.session_id();
        {
            let calls = calls.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].0, "$ErrorActionPreference = 'Stop'\nGet-ChildItem");
            let env = &calls[0].1;
            if expose {
                assert_eq!(env["PI_SESSION_ID"], id);
                assert_eq!(env["PI_PROVIDER"], "faux");
                assert_eq!(env["PI_MODEL"], "faux-1");
                assert_eq!(env["PI_REASONING_LEVEL"], "off");
                assert!(env.contains_key("PI_SESSION_FILE"));
            } else {
                assert!(!env.keys().any(|k| matches!(
                    k.as_str(),
                    "PI_SESSION_ID"
                        | "PI_SESSION_FILE"
                        | "PI_PROVIDER"
                        | "PI_MODEL"
                        | "PI_REASONING_LEVEL"
                )));
            }
        }
        result.session.shutdown().await.unwrap();
    }
}

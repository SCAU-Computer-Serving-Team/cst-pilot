#[cfg(windows)]
#[path = "settings_and_auth/command_timeout.rs"]
mod command_timeout;

use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

use pi_ai::{ApiKeyCredential, Credential, CredentialStore};
use pi_runtime::{
    auth_storage::{FileAuthStorage, ReadOnlyAuthStorage, RuntimeCredentials, StoredAuthStatus},
    config_value::{ConfigValueResolver, ConfigValueSource, is_config_value_configured},
    settings_manager::{SettingsManager, SettingsScope},
};
use serde_json::{Value, json};

fn root() -> PathBuf {
    let p = PathBuf::from("E:/tmp/2026-11-04/argon-phase4-development/tests")
        .join(format!("auth-{}", pi_ai::uuid_v7()));
    std::fs::create_dir_all(&p).unwrap();
    p
}
fn key(value: &str) -> Credential {
    Credential::ApiKey(ApiKeyCredential {
        key: Some(value.into()),
        env: None,
    })
}
async fn put(store: &dyn CredentialStore, provider: &str, value: &str) {
    let credential = key(value);
    store
        .modify(
            provider,
            Box::new(move |_| Box::pin(async move { Ok(Some(credential)) })),
        )
        .await
        .unwrap();
}
fn patch(settings: &SettingsManager, value: Value) -> Result<(), String> {
    settings.patch_scope(SettingsScope::Global, value.as_object().unwrap().clone())
}

#[tokio::test]
async fn async_auth_lock_wait_does_not_block_executor_and_rereads_latest() {
    let p = root().join("auth.json");
    let a = FileAuthStorage::new(&p, ConfigValueResolver::default());
    let b = FileAuthStorage::new(&p, ConfigValueResolver::default());
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let first = tokio::spawn(async move {
        a.modify(
            "one",
            Box::new(move |_| {
                Box::pin(async move {
                    entered_tx.send(()).unwrap();
                    release_rx.await.unwrap();
                    Ok(Some(key("mock-one")))
                })
            }),
        )
        .await
        .unwrap();
    });
    entered_rx.await.unwrap();
    let second = tokio::spawn(async move {
        put(&b, "two", "mock-two").await;
    });
    tokio::task::yield_now().await;
    release_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        first.await.unwrap();
        second.await.unwrap();
    })
    .await
    .unwrap();
    let v: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
    assert!(v.get("one").is_some() && v.get("two").is_some());
}

#[tokio::test]
async fn dropped_auth_modify_never_commits_and_releases_file_lock() {
    let p = root().join("auth.json");
    let store = FileAuthStorage::new(&p, ConfigValueResolver::default());
    put(&store, "one", "original").await;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let worker = tokio::spawn(async move {
        store
            .modify(
                "one",
                Box::new(move |_| {
                    Box::pin(async move {
                        tx.send(()).unwrap();
                        std::future::pending::<()>().await;
                        Ok(Some(key("must-not-commit")))
                    })
                }),
            )
            .await
    });
    rx.await.unwrap();
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    let store = FileAuthStorage::new(&p, ConfigValueResolver::default());
    tokio::time::timeout(Duration::from_secs(2), put(&store, "two", "next"))
        .await
        .unwrap();
    let v: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
    assert_eq!(v["one"]["key"], "original");
    assert_eq!(v["two"]["key"], "next");
}

#[tokio::test]
async fn auth_update_preserves_unknown_fields_and_other_providers() {
    let p = root().join("auth.json");
    std::fs::write(&p, json!({"one":{"type":"api_key","key":"old","extension":{"keep":true}},"two":{"type":"api_key","key":"other","custom":7}}).to_string()).unwrap();
    let store = FileAuthStorage::new(&p, ConfigValueResolver::default());
    put(&store, "one", "new").await;
    let v: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
    assert_eq!(v["one"]["extension"]["keep"], true);
    assert_eq!(v["two"]["custom"], 7);
    assert!(p.with_file_name("auth.json.lock").exists());
}

#[tokio::test]
async fn auth_malformed_file_cannot_be_overwritten_and_errors_are_redacted() {
    let p = root().join("auth.json");
    let raw = r#"{"mock-secret-provider-command": {"type":"api_key","key":42}}"#;
    std::fs::write(&p, raw).unwrap();
    let store = FileAuthStorage::new(&p, ConfigValueResolver::default());
    let error = store
        .modify(
            "x",
            Box::new(|_| Box::pin(async { Ok(Some(key("replacement"))) })),
        )
        .await
        .unwrap_err();
    assert!(!error.contains("mock-secret"), "{error}");
    assert_eq!(std::fs::read_to_string(&p).unwrap(), raw);
    assert!(store.delete("x").await.is_err());
}

#[tokio::test]
async fn readonly_commands_and_runtime_overrides_never_touch_disk_or_execute() {
    let dir = root();
    let p = dir.join("auth.json");
    let marker = dir.join("executed");
    let raw = json!({"one":{"type":"api_key","key":format!("!echo mock > {}", marker.display())}})
        .to_string();
    std::fs::write(&p, &raw).unwrap();
    let readonly = ReadOnlyAuthStorage::new(&p, ConfigValueResolver::default());
    assert_eq!(
        readonly.check_stored_status("one").unwrap(),
        StoredAuthStatus::StoredCommand
    );
    let Credential::ApiKey(c) = readonly.read("one").await.unwrap().unwrap() else {
        panic!()
    };
    assert!(c.key.is_none());
    assert!(!marker.exists());
    let runtime = RuntimeCredentials::new(Arc::new(readonly), ConfigValueResolver::default());
    runtime.set_runtime_api_key("one", "mock-private-override");
    assert_eq!(runtime.list().await.unwrap().len(), 1);
    assert!(!format!("{runtime:?}").contains("mock-private-override"));
    runtime.remove_runtime_api_key("one");
    assert_eq!(std::fs::read_to_string(p).unwrap(), raw);
    assert!(!marker.exists());
}

#[cfg(windows)]
#[tokio::test]
async fn command_credentials_use_instance_cache_and_redact_failures() {
    let dir = root();
    let marker = dir.join("count.txt");
    let command = format!(
        "!echo tick>>\"{}\" & echo mock-command-value",
        marker.display()
    );
    let p = dir.join("auth.json");
    std::fs::write(
        &p,
        json!({"one":{"type":"api_key","key":command}}).to_string(),
    )
    .unwrap();
    let resolver = ConfigValueResolver::new(&dir, &dir, true);
    let store = FileAuthStorage::new(&p, resolver.clone());
    for _ in 0..2 {
        let Some(Credential::ApiKey(api)) = store.read("one").await.unwrap() else {
            panic!()
        };
        assert_eq!(api.key.as_deref(), Some("mock-command-value"));
    }
    assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 1);
    resolver.clear_cache();
    store.read("one").await.unwrap();
    assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 2);
    let error = resolver
        .resolve_or_throw(
            "!echo mock-secret-output & exit /b 7",
            "credential",
            None,
            ConfigValueSource::Global,
            None,
        )
        .unwrap_err();
    assert!(error.contains("exit code: 7"));
    assert!(!error.contains("mock-secret"));
    assert!(!format!("{store:?} {resolver:?}").contains("mock-command"));
}

#[cfg(windows)]
#[tokio::test]
async fn cache_clear_cancels_inflight_command_without_blocking_executor() {
    let dir = root();
    let marker = dir.join("started.txt");
    let command = format!(
        "!powershell.exe -NoLogo -NoProfile -NonInteractive -Command \"if (Test-Path '{}') {{ 'mock-new'; exit }}; [System.IO.File]::WriteAllText('{}','started'); Start-Sleep -Seconds 30; 'mock-old'\"",
        marker.display(),
        marker.display()
    );
    let resolver = ConfigValueResolver::new(&dir, &dir, true);
    let clone = resolver.clone();
    let first_command = command.clone();
    let task = tokio::spawn(async move {
        clone
            .resolve_async(first_command, None, ConfigValueSource::Global)
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut interval = tokio::time::interval(Duration::from_millis(10));
        while !marker.exists() {
            interval.tick().await;
        }
    })
    .await
    .unwrap();
    resolver.clear_cache();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(
        resolver
            .resolve_async(command, None, ConfigValueSource::Global)
            .await
            .unwrap()
            .as_deref(),
        Some("mock-new")
    );
}

#[test]
fn settings_patch_detected_corruption_stays_memory_only_until_reload() {
    let dir = root();
    let settings = SettingsManager::create(&dir, dir.join("agent"));
    let p = dir.join("agent/settings.json");
    patch(&settings, json!({"defaultModel":"old"})).unwrap();
    std::fs::write(&p, "{mock-secret-broken").unwrap();
    assert!(patch(&settings, json!({"defaultModel":"first"})).is_err());
    std::fs::write(&p, "{\"defaultModel\":\"repaired\"}").unwrap();
    patch(&settings, json!({"defaultModel":"memory-only"})).unwrap();
    assert_eq!(settings.get_default_model().as_deref(), Some("memory-only"));
    let v: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
    assert_eq!(v["defaultModel"], "repaired");
    settings.reload();
    assert_eq!(settings.get_default_model().as_deref(), Some("repaired"));
    patch(&settings, json!({"defaultModel":"saved"})).unwrap();
    assert!(!format!("{:?}", settings.get_errors()).contains("mock-secret"));
}

#[test]
fn same_scope_tool_patch_replaces_array_and_project_modifiers_append() {
    let dir = root();
    let settings = SettingsManager::create(&dir, dir.join("agent"));
    patch(
        &settings,
        json!({"defaultTools":["ls"],"extension":{"left":1,"right":2}}),
    )
    .unwrap();
    patch(
        &settings,
        json!({"defaultTools":["+grep"],"extension":{"left":3}}),
    )
    .unwrap();
    assert_eq!(
        settings.get_default_tools().unwrap(),
        ["read", "bash", "edit", "write", "grep"]
    );
    settings
        .patch_scope(
            SettingsScope::Project,
            json!({"defaultTools":["-bash","+ls"]})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
    assert_eq!(
        settings.get_default_tools().unwrap(),
        ["read", "edit", "write", "grep", "ls"]
    );
    assert_eq!(settings.get_raw_effective()["extension"]["right"], 2);
}

#[test]
fn settings_reject_empty_and_truncated_comments_and_do_not_join_tokens() {
    for raw in ["", "{} /*unfinished", "{\"number\":1/*comment*/2}"] {
        let dir = root();
        std::fs::write(dir.join("settings.json"), raw).unwrap();
        let settings = SettingsManager::create(&dir, &dir);
        assert!(!settings.get_errors().is_empty(), "accepted {raw:?}");
        patch(&settings, json!({"defaultModel":"memory"})).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("settings.json")).unwrap(),
            raw
        );
    }
}

#[test]
fn untrusted_project_filters_executable_paths_and_keeps_preferences() {
    let dir = root();
    std::fs::create_dir_all(dir.join(".pi")).unwrap();
    std::fs::write(
        dir.join("settings.json"),
        json!({"shellPath":"global-safe","extension":{"left":1}}).to_string(),
    )
    .unwrap();
    std::fs::write(dir.join(".pi/settings.json"), json!({"shellPath":"project-shell","shellCommandPrefix":"mock-private-command","extensions":["project"],"packages":["project"],"defaultModel":"project-model","extension":{"right":2}}).to_string()).unwrap();
    let settings = SettingsManager::create_with_trust(&dir, &dir, false);
    assert_eq!(settings.get_shell_path().as_deref(), Some("global-safe"));
    assert!(settings.get_shell_command_prefix().is_none());
    assert!(settings.get_raw_effective().get("extensions").is_none());
    assert_eq!(
        settings.get_default_model().as_deref(),
        Some("project-model")
    );
    assert_eq!(
        settings.get_raw_effective()["extension"],
        json!({"left":1,"right":2})
    );
    assert!(!format!("{settings:?}").contains("mock-private"));
    settings.set_project_trusted(true);
    assert_eq!(settings.get_shell_path().as_deref(), Some("project-shell"));
}

#[tokio::test]
async fn multiprocess_storage_child() {
    let Some(dir) = std::env::var_os("PI_PHASE4_STORAGE_CHILD_DIR") else {
        return;
    };
    let id = std::env::var("PI_PHASE4_STORAGE_CHILD_ID").unwrap();
    let dir = PathBuf::from(dir);
    let settings = SettingsManager::create(&dir, &dir);
    let store = FileAuthStorage::new(dir.join("auth.json"), ConfigValueResolver::default());
    for i in 0..8 {
        let name = format!("child-{id}-{i}");
        patch(&settings, json!({"extension":{&name:i}})).unwrap();
        put(&store, &name, "mock-child").await;
        store
            .modify(
                "oauth-counter",
                Box::new(|current| {
                    Box::pin(async move {
                        let Some(Credential::OAuth(mut oauth)) = current else {
                            panic!("missing fixture")
                        };
                        tokio::task::yield_now().await;
                        let count = oauth.extra["counter"].as_u64().unwrap();
                        oauth.extra.insert("counter".into(), json!(count + 1));
                        Ok(Some(Credential::OAuth(oauth)))
                    })
                }),
            )
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn multiprocess_auth_oauth_and_settings_preserve_every_update() {
    let dir = root();
    std::fs::write(
        dir.join("settings.json"),
        json!({"unknown":{"keep":true}}).to_string(),
    )
    .unwrap();
    std::fs::write(dir.join("auth.json"), json!({"oauth-counter":{"type":"oauth","access":"mock-access","refresh":"mock-refresh","expires":0,"counter":0,"unknown":"keep"}}).to_string()).unwrap();
    let mut children = tokio::task::JoinSet::new();
    for id in 0..4 {
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "multiprocess_storage_child", "--nocapture"])
            .env_clear()
            .env("PI_PHASE4_STORAGE_CHILD_DIR", &dir)
            .env("PI_PHASE4_STORAGE_CHILD_ID", id.to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", system_root);
        }
        let child = command.spawn().unwrap();
        children.spawn(async move {
            let output = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
                .await
                .unwrap()
                .unwrap();
            assert!(
                output.status.success(),
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        });
    }
    while let Some(result) = children.join_next().await {
        result.unwrap();
    }
    let auth: Value =
        serde_json::from_slice(&std::fs::read(dir.join("auth.json")).unwrap()).unwrap();
    let settings: Value =
        serde_json::from_slice(&std::fs::read(dir.join("settings.json")).unwrap()).unwrap();
    assert_eq!(auth.as_object().unwrap().len(), 33);
    assert_eq!(auth["oauth-counter"]["counter"], 32);
    assert_eq!(auth["oauth-counter"]["unknown"], "keep");
    assert_eq!(settings["extension"].as_object().unwrap().len(), 32);
    assert_eq!(settings["unknown"]["keep"], true);
}

#[test]
fn config_values_handle_escapes_missing_env_empty_values_and_untrusted_sources() {
    let dir = root();
    let resolver = ConfigValueResolver::new(&dir, &dir, false);
    let env = BTreeMap::from([("PHASE4_LOCAL".into(), "mock".into())]);
    assert_eq!(
        resolver
            .resolve(
                "$$ $! ${PHASE4_LOCAL}/$PHASE4_LOCAL",
                Some(&env),
                ConfigValueSource::Global,
                None
            )
            .as_deref(),
        Some("$ ! mock/mock")
    );
    assert!(!is_config_value_configured("", None));
    assert!(!is_config_value_configured("$PHASE4_MISSING_46327", None));
    let marker = dir.join("executed");
    let command = format!("!echo mock > {}", marker.display());
    for source in [
        ConfigValueSource::Project,
        ConfigValueSource::Extension { from_project: true },
    ] {
        let error = resolver
            .resolve_or_throw(&command, "credential", None, source, None)
            .unwrap_err();
        assert!(!error.contains("echo"));
        assert!(error.contains("untrusted"));
    }
    assert!(!marker.exists());
}

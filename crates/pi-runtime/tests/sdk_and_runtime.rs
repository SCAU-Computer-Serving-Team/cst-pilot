#[path = "sdk_and_runtime/image_filtering.rs"]
mod image_filtering;

#[path = "sdk_and_runtime/cache_warming.rs"]
mod cache_warming;
#[path = "sdk_and_runtime/cache_warming_sdk.rs"]
mod cache_warming_sdk;
#[path = "sdk_and_runtime/lifecycle_followup.rs"]
mod lifecycle_followup;

use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use pi_agent::AgentMessage;
use pi_ai::{Models, UserMessage};
use pi_session::{AgentSession, AgentSessionOptions, InlineExtension, SessionManager};
use tokio::sync::Notify;

async fn runtime_services() -> pi_runtime::AgentSessionServices {
    use pi_runtime::*;
    let root = PathBuf::from("E:/tmp/2026-11-04/argon-phase4-development/tests")
        .join(format!("runtime-{}", pi_ai::uuid_v7()));
    std::fs::create_dir_all(&root).unwrap();
    create_agent_session_services(CreateAgentSessionServicesOptions {
        cwd: root.clone(),
        agent_dir: Some(root.join("agent")),
        model_runtime: Some(
            ModelRuntime::create(CreateModelRuntimeOptions {
                include_builtins: Some(false),
                refresh_on_create: Some(false),
                ..Default::default()
            })
            .await,
        ),
        resource_loader_options: Some(DefaultResourceLoaderOptions {
            no_extensions: true,
            no_skills: true,
            no_context_files: true,
            no_prompt_templates: true,
            ..Default::default()
        }),
        ..Default::default()
    })
    .await
}

fn runtime_result(
    services: pi_runtime::AgentSessionServices,
    sm: SessionManager,
    extensions: Vec<Box<dyn pi_session::Extension>>,
) -> pi_runtime::CreateAgentSessionRuntimeResult {
    let mut options = AgentSessionOptions::new_unconfigured(
        services.cwd.to_string_lossy(),
        services.model_runtime.models().clone(),
        sm,
    );
    options.extensions = extensions;
    pi_runtime::CreateAgentSessionRuntimeResult {
        session: AgentSession::new(options).unwrap(),
        extensions_result: services.resource_loader.get_extensions(),
        services,
        diagnostics: vec![],
        model_fallback_message: None,
    }
}

fn runtime_factory(
    services: pi_runtime::AgentSessionServices,
) -> pi_runtime::CreateAgentSessionRuntimeFactory {
    Arc::new(move |options| {
        let mut services = services.clone();
        Box::pin(async move {
            services.cwd = options.cwd;
            Ok(runtime_result(services, options.session_manager, vec![]))
        })
    })
}

fn runtime_manager(services: &pi_runtime::AgentSessionServices) -> SessionManager {
    let mut sm = SessionManager::create(
        services.cwd.to_string_lossy(),
        services.cwd.join("sessions"),
        None,
    )
    .unwrap();
    sm.append_message(AgentMessage::User(UserMessage::new("persisted")))
        .unwrap();
    sm
}

fn persistent_session(options: impl FnOnce(&mut AgentSessionOptions)) -> (AgentSession, PathBuf) {
    let dir = PathBuf::from("E:/tmp/2026-11-04/argon-phase4-development/tests")
        .join(format!("shutdown-{}", pi_ai::uuid_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let cwd = dir.to_string_lossy().into_owned();
    let mut sm = SessionManager::create(&cwd, &dir, None).unwrap();
    sm.append_message(AgentMessage::User(UserMessage::new("persisted")))
        .unwrap();
    let path = sm.get_session_file().unwrap().to_path_buf();
    let mut opts = AgentSessionOptions::new_unconfigured(cwd, Arc::new(Models::default()), sm);
    options(&mut opts);
    (AgentSession::new(opts).unwrap(), path)
}

#[tokio::test]
async fn dropping_shutdown_waiter_does_not_cancel_shared_close() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let (session, path) = persistent_session(|opts| {
        let (entered, release, shutdowns) = (entered.clone(), release.clone(), shutdowns.clone());
        opts.extensions.push(Box::new(InlineExtension::new(
            "<shutdown-test>",
            move |api| {
                let (entered, release, shutdowns) =
                    (entered.clone(), release.clone(), shutdowns.clone());
                api.on_lifecycle("session_shutdown", move |_, _| {
                    let (entered, release, shutdowns) =
                        (entered.clone(), release.clone(), shutdowns.clone());
                    async move {
                        shutdowns.fetch_add(1, Ordering::SeqCst);
                        entered.notify_one();
                        release.notified().await;
                        Ok(pi_session::SessionBeforeEventOutput::None)
                    }
                });
                Ok(())
            },
        )));
    });
    let old_clone = session.clone();
    let closer = tokio::spawn(async move { old_clone.shutdown().await });
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    closer.abort();
    let _ = closer.await;
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !session.is_closed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("closing task must survive a dropped caller");
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    session.shutdown().await.unwrap();
    assert!(
        SessionManager::open(&path, None, None).is_ok(),
        "retained clone must not retain store lock"
    );
}

#[tokio::test]
async fn idle_dispose_uses_the_same_shutdown_lifecycle() {
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let closing = Arc::new(AtomicUsize::new(0));
    let (session, path) = persistent_session(|opts| {
        let closing = closing.clone();
        opts.on_session_closing = Some(Arc::new(move || {
            closing.fetch_add(1, Ordering::SeqCst);
        }));
        let shutdowns = shutdowns.clone();
        opts.extensions.push(Box::new(InlineExtension::new(
            "<dispose-test>",
            move |api| {
                let shutdowns = shutdowns.clone();
                api.on_lifecycle("session_shutdown", move |_, _| {
                    shutdowns.fetch_add(1, Ordering::SeqCst);
                    async { Ok(pi_session::SessionBeforeEventOutput::None) }
                });
                Ok(())
            },
        )));
    });
    session.dispose();
    session.shutdown().await.unwrap();
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    assert_eq!(closing.load(Ordering::SeqCst), 1);
    assert!(SessionManager::open(path, None, None).is_ok());
}

#[tokio::test]
async fn concurrent_close_shares_failure_and_emits_one_shutdown() {
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let (session, path) = persistent_session(|opts| {
        let shutdowns = shutdowns.clone();
        opts.extensions.push(Box::new(InlineExtension::new(
            "<close-failure-test>",
            move |api| {
                let shutdowns = shutdowns.clone();
                api.on_lifecycle("session_shutdown", move |_, _| {
                    shutdowns.fetch_add(1, Ordering::SeqCst);
                    async { Ok(pi_session::SessionBeforeEventOutput::None) }
                });
                Ok(())
            },
        )));
    });
    session.set_fatal_runtime_error("injected storage failure");
    let (first, second) = tokio::join!(session.shutdown(), session.shutdown());
    assert_eq!(first, Err("injected storage failure".to_owned()));
    assert_eq!(first, second);
    assert_eq!(session.shutdown().await, first);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    assert!(SessionManager::open(path, None, None).is_ok());
}

#[tokio::test]
async fn shutdown_callback_reentrant_wait_is_rejected() {
    let holder = Arc::new(std::sync::Mutex::new(None::<AgentSession>));
    let (session, _) = persistent_session(|opts| {
        let holder = holder.clone();
        opts.extensions.push(Box::new(InlineExtension::new(
            "<reentrant-close-test>",
            move |api| {
                let holder = holder.clone();
                api.on_lifecycle("session_shutdown", move |_, _| {
                    let session = holder.lock().unwrap().as_ref().unwrap().clone();
                    async move {
                        assert!(
                            session
                                .shutdown()
                                .await
                                .unwrap_err()
                                .contains("own session callback")
                        );
                        session.dispose();
                        Ok(pi_session::SessionBeforeEventOutput::None)
                    }
                });
                Ok(())
            },
        )));
    });
    *holder.lock().unwrap() = Some(session.clone());
    tokio::time::timeout(Duration::from_secs(3), session.shutdown())
        .await
        .unwrap()
        .unwrap();
    holder.lock().unwrap().take();
}

#[tokio::test]
async fn none_model_does_not_revive_historical_identity() {
    let (session, _) = persistent_session(|opts| {
        let reg = pi_ai::faux::register_faux_provider(&opts.models);
        opts.session_manager
            .append_model_change(&reg.model.provider, &reg.model.id)
            .unwrap();
        opts.model = None;
    });
    assert!(session.configured_model().is_none());
    assert!(session.prompt("needs model", None).await.is_err());
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn closing_rejects_mutator_callbacks_from_old_clones() {
    let (session, _) = persistent_session(|_| {});
    let old = session.clone();
    session.dispose();
    assert!(
        old.with_session_manager_mut(|_| {
            panic!("closed session manager callback must not run");
            #[allow(unreachable_code)]
            Ok(())
        })
        .unwrap_err()
        .to_string()
        .contains("AgentSession is closed")
    );
    assert!(
        old.with_execution_log_mut(|_| {
            panic!("closed execution log callback must not run");
            #[allow(unreachable_code)]
            Ok(())
        })
        .unwrap_err()
        .to_string()
        .contains("AgentSession is closed")
    );
    session.shutdown().await.unwrap();
    assert!(old.set_session_name("closed").is_err());
    assert!(
        old.prompt("closed", None)
            .await
            .unwrap_err()
            .contains("AgentSession is closed")
    );
}

#[tokio::test]
async fn shutdown_waits_for_started_tool_and_persists_its_result() {
    use pi_ai::faux::{
        FauxResponseStep, faux_assistant_message, faux_tool_call, register_faux_provider,
    };
    use pi_session::ToolDefinition;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (session, path) = persistent_session(|opts| {
        let reg = register_faux_provider(&opts.models);
        opts.model = Some(reg.model.clone());
        reg.set_responses(vec![FauxResponseStep::Static(faux_assistant_message(
            vec![faux_tool_call(
                "settle",
                Default::default(),
                Some("settle-call".to_owned()),
            )],
            Some(pi_ai::StopReason::ToolUse),
        ))]);
        let (entered, release) = (entered.clone(), release.clone());
        opts.base_tools.push(ToolDefinition::new(
            "settle",
            "Settle",
            "Wait for disk settlement",
            serde_json::json!({"type":"object", "properties":{}}),
            Arc::new(move |_, _, _, _, _| {
                let (entered, release) = (entered.clone(), release.clone());
                Box::pin(async move {
                    entered.notify_one();
                    release.notified().await;
                    Ok(pi_agent::AgentToolResult::text("settled result"))
                })
            }),
        ));
        opts.initial_active_tool_names = Some(vec!["settle".to_owned()]);
    });
    let s = session.clone();
    let running = tokio::spawn(async move { s.prompt("run tool", None).await });
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    running.abort();
    let _ = running.await;
    session.dispose();
    assert!(!session.is_closed());
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), session.shutdown())
        .await
        .unwrap()
        .unwrap();
    let sm = SessionManager::open(path, None, None).unwrap();
    let text = serde_json::to_string(&sm.get_entries()).unwrap();
    assert!(text.contains("settled result"));
    assert!(text.contains("settle-call"));
}

#[tokio::test]
async fn dropped_tree_waiter_is_cancelled_by_shutdown() {
    let entered = Arc::new(Notify::new());
    let (session, path) = persistent_session(|opts| {
        opts.session_manager
            .append_message(AgentMessage::User(UserMessage::new("second")))
            .unwrap();
        let entered = entered.clone();
        opts.extensions.push(Box::new(InlineExtension::new(
            "<tree-close-test>",
            move |api| {
                let entered = entered.clone();
                api.on_lifecycle("session_before_tree", move |_, _| {
                    let entered = entered.clone();
                    async move {
                        entered.notify_one();
                        std::future::pending::<()>().await;
                        Ok(pi_session::SessionBeforeEventOutput::None)
                    }
                });
                Ok(())
            },
        )));
    });
    let target = session.with_session_manager(|sm| sm.get_entries()[0].id().to_owned());
    let s = session.clone();
    let caller = tokio::spawn(async move { s.navigate_tree(&target, Default::default()).await });
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    caller.abort();
    let _ = caller.await;
    tokio::time::timeout(Duration::from_secs(3), session.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(SessionManager::open(path, None, None).is_ok());
}

#[tokio::test]
async fn dropped_compaction_waiter_is_cancelled_by_shutdown() {
    let entered = Arc::new(Notify::new());
    let (session, path) = persistent_session(|opts| {
        let reg = pi_ai::faux::register_faux_provider(&opts.models);
        opts.model = Some(reg.model.clone());
        for _ in 0..12 {
            opts.session_manager
                .append_message(AgentMessage::User(UserMessage::new("message ".repeat(300))))
                .unwrap();
            opts.session_manager
                .append_message(AgentMessage::Assistant(pi_ai::faux::faux_text_message(
                    "answer ".repeat(300),
                )))
                .unwrap();
        }
        let entered = entered.clone();
        opts.extensions.push(Box::new(InlineExtension::new(
            "<compaction-close-test>",
            move |api| {
                let entered = entered.clone();
                api.on_lifecycle("session_before_compact", move |_, _| {
                    let entered = entered.clone();
                    async move {
                        entered.notify_one();
                        std::future::pending::<()>().await;
                        Ok(pi_session::SessionBeforeEventOutput::None)
                    }
                });
                Ok(())
            },
        )));
    });
    let s = session.clone();
    let caller = tokio::spawn(async move { s.compact(None).await });
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    caller.abort();
    let _ = caller.await;
    tokio::time::timeout(Duration::from_secs(3), session.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(SessionManager::open(path, None, None).is_ok());
}

#[tokio::test]
async fn dropping_bash_caller_still_settles_and_releases_store() {
    use pi_session::{BashResult, ExecuteBashOptions};
    let entered = Arc::new(Notify::new());
    let (session, path) = persistent_session(|opts| {
        let entered = entered.clone();
        opts.default_bash_executor = Some(Arc::new(move |req| {
            let entered = entered.clone();
            Box::pin(async move {
                entered.notify_one();
                req.cancel_token.cancelled().await;
                Ok(BashResult {
                    output: "cancelled and settled".to_owned(),
                    cancelled: true,
                    ..Default::default()
                })
            })
        }));
    });
    let s = session.clone();
    let caller =
        tokio::spawn(async move { s.execute_bash("mock", ExecuteBashOptions::default()).await });
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    caller.abort();
    let _ = caller.await;
    tokio::time::timeout(Duration::from_secs(3), session.shutdown())
        .await
        .expect("dropped Bash caller must not leak active-operation registration")
        .unwrap();
    let sm = SessionManager::open(path, None, None).unwrap();
    assert!(
        serde_json::to_string(&sm.get_entries())
            .unwrap()
            .contains("cancelled and settled")
    );
}

#[tokio::test]
async fn runtime_dropped_replacement_waiter_does_not_leave_closed_active_slot() {
    use pi_runtime::*;
    let services = runtime_services().await;
    let initial = runtime_result(services.clone(), runtime_manager(&services), vec![]);
    let old = initial.session.clone();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let factory: CreateAgentSessionRuntimeFactory = {
        let (entered, release) = (entered.clone(), release.clone());
        Arc::new(move |options| {
            let (entered, release, mut services) =
                (entered.clone(), release.clone(), services.clone());
            Box::pin(async move {
                entered.notify_one();
                release.notified().await;
                services.cwd = options.cwd;
                Ok(runtime_result(services, options.session_manager, vec![]))
            })
        })
    };
    let host = AgentSessionRuntime::new(initial, factory);
    let pending = {
        let host = host.clone();
        tokio::spawn(async move { host.new_session(None).await })
    };
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    assert!(old.is_closed());
    pending.abort();
    let _ = pending.await;
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !host.try_session().is_ok_and(|s| !s.is_closed()) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("detached replacement must finish after caller drop");
    host.dispose().await.unwrap();
}

#[tokio::test]
async fn runtime_target_execution_log_preflight_failure_preserves_old_session() {
    use pi_runtime::*;
    let services = runtime_services().await;
    let initial = runtime_result(services.clone(), runtime_manager(&services), vec![]);
    let old = initial.session.clone();
    let mut target = runtime_manager(&services);
    let path = target.get_session_file().unwrap().to_path_buf();
    target.release_persistent_store();
    std::fs::write(format!("{}.state.json", path.display()), b"corrupt log").unwrap();
    let host = AgentSessionRuntime::new(
        initial,
        Arc::new(|_| Box::pin(async { Err("factory must not run".into()) })),
    );
    assert!(host.switch_session(&path, None).await.is_err());
    assert!(
        !old.is_closing_or_closed(),
        "preflight must finish before tearing down the old session"
    );
    assert!(!host.is_failed());
    old.set_session_name("still active").unwrap();
    host.dispose().await.unwrap();
}

#[tokio::test]
async fn runtime_dispose_cancels_pending_before_hook_without_teardown_race() {
    use pi_runtime::*;
    let services = runtime_services().await;
    let entered = Arc::new(Notify::new());
    let extension = {
        let entered = entered.clone();
        InlineExtension::new("<pending-before>", move |api| {
            let entered = entered.clone();
            api.on_lifecycle("session_before_switch", move |_, _| {
                let entered = entered.clone();
                async move {
                    entered.notify_one();
                    std::future::pending::<()>().await;
                    Ok(pi_session::SessionBeforeEventOutput::None)
                }
            });
            Ok(())
        })
    };
    let initial = runtime_result(
        services.clone(),
        runtime_manager(&services),
        vec![Box::new(extension)],
    );
    let old = initial.session.clone();
    let host = AgentSessionRuntime::new(
        initial,
        Arc::new(|_| Box::pin(async { Err("factory must not run".into()) })),
    );
    let pending = {
        let host = host.clone();
        tokio::spawn(async move { host.new_session(None).await })
    };
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), host.dispose())
        .await
        .unwrap()
        .unwrap();
    assert!(pending.await.unwrap().is_err());
    assert!(old.is_closed());
    assert!(host.is_disposed());
}

#[tokio::test]
async fn runtime_setup_runs_after_commit_and_outside_transition_lock() {
    use pi_runtime::*;
    let services = runtime_services().await;
    let initial = runtime_result(services.clone(), runtime_manager(&services), vec![]);
    let factory: CreateAgentSessionRuntimeFactory = Arc::new(move |options| {
        let mut services = services.clone();
        Box::pin(async move {
            services.cwd = options.cwd;
            Ok(runtime_result(services, options.session_manager, vec![]))
        })
    });
    let host = AgentSessionRuntime::new(initial, factory);
    let nested = host.clone();
    let setup: SetupSessionManagerFn = Arc::new(move |_| {
        let host = nested.clone();
        Box::pin(async move {
            host.new_session(None).await?;
            Ok(())
        })
    });
    tokio::time::timeout(
        Duration::from_secs(3),
        host.new_session(Some(NewSessionRuntimeOptions {
            setup: Some(setup),
            ..Default::default()
        })),
    )
    .await
    .expect("setup must not hold structural lock")
    .unwrap();
    host.dispose().await.unwrap();
}

#[tokio::test]
async fn runtime_switch_preflight_errors_leave_old_open_and_locked() {
    use pi_runtime::*;
    let services = runtime_services().await;
    let initial = runtime_result(services.clone(), runtime_manager(&services), vec![]);
    let old = initial.session.clone();
    let original = old.session_file().unwrap();
    let host = AgentSessionRuntime::new(initial, runtime_factory(services.clone()));
    let missing = services.cwd.join("missing.jsonl");
    assert!(host.switch_session(missing, None).await.is_err());
    let locked = runtime_manager(&services);
    assert!(
        host.switch_session(locked.get_session_file().unwrap(), None)
            .await
            .is_err()
    );
    let bad_cwd = services.cwd.join("does-not-exist");
    let mut target =
        SessionManager::create(bad_cwd.to_string_lossy(), services.cwd.join("other"), None)
            .unwrap();
    target
        .append_message(AgentMessage::User(UserMessage::new("bad cwd")))
        .unwrap();
    let path = target.get_session_file().unwrap().to_path_buf();
    target.release_persistent_store();
    assert!(
        host.switch_session(path, None)
            .await
            .unwrap_err()
            .contains("working directory")
    );
    assert!(!old.is_closing_or_closed());
    assert_eq!(host.session().session_file().unwrap(), original);
    assert!(SessionManager::open(&original, None, None).is_err());
    host.dispose().await.unwrap();
    assert!(SessionManager::open(&original, None, None).is_ok());
}

#[tokio::test]
async fn runtime_same_file_reopen_releases_retained_clone_and_recovers_after_failure() {
    use pi_runtime::*;
    let services = runtime_services().await;
    let initial = runtime_result(services.clone(), runtime_manager(&services), vec![]);
    let old = initial.session.clone();
    let original = old.session_file().unwrap();
    let host = AgentSessionRuntime::new(initial, runtime_factory(services));
    host.switch_session(&original, None).await.unwrap();
    assert!(old.is_closed());
    assert!(old.set_session_name("rejected").is_err());
    assert_eq!(host.session().session_file().unwrap(), original);
    let retained = host.session();
    // A failure after old-store release must enter Failed, then permit explicit recovery.
    let remove = original.clone();
    retained.set_before_session_invalidate(Some(Arc::new(move || {
        std::fs::remove_file(&remove).unwrap();
    })));
    // Runtime owns this hook; install it on the host too.
    let remove = original.clone();
    host.set_before_session_invalidate(Some(Arc::new(move || {
        std::fs::remove_file(&remove).unwrap();
    })));
    assert!(host.switch_session(&original, None).await.is_err());
    assert!(retained.is_closed());
    assert!(host.is_failed());
    assert!(host.try_session().is_err());
    host.set_before_session_invalidate(None);
    host.new_session(None).await.unwrap();
    assert!(!host.is_failed());
    host.dispose().await.unwrap();
}

#[tokio::test]
async fn runtime_factory_failure_releases_target_lock_and_allows_recovery() {
    use pi_runtime::*;
    let services = runtime_services().await;
    let initial = runtime_result(services.clone(), runtime_manager(&services), vec![]);
    let old = initial.session.clone();
    let mut target = runtime_manager(&services);
    let path = target.get_session_file().unwrap().to_path_buf();
    target.release_persistent_store();
    let calls = Arc::new(AtomicUsize::new(0));
    let factory: CreateAgentSessionRuntimeFactory = {
        let calls = calls.clone();
        Arc::new(move |options| {
            let (calls, mut services) = (calls.clone(), services.clone());
            Box::pin(async move {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err("injected factory failure".into());
                }
                services.cwd = options.cwd;
                Ok(runtime_result(services, options.session_manager, vec![]))
            })
        })
    };
    let host = AgentSessionRuntime::new(initial, factory);
    assert!(host.switch_session(&path, None).await.is_err());
    assert!(old.is_closed());
    assert!(host.is_failed());
    assert!(SessionManager::open(&path, None, None).is_ok());
    host.switch_session(&path, None).await.unwrap();
    host.dispose().await.unwrap();
}

#[tokio::test]
async fn runtime_dispose_cancels_pending_factory_and_releases_target_lock() {
    use pi_runtime::*;
    let services = runtime_services().await;
    let initial = runtime_result(services.clone(), runtime_manager(&services), vec![]);
    let old = initial.session.clone();
    let mut target = runtime_manager(&services);
    let path = target.get_session_file().unwrap().to_path_buf();
    target.release_persistent_store();
    let entered = Arc::new(Notify::new());
    let factory: CreateAgentSessionRuntimeFactory = {
        let entered = entered.clone();
        Arc::new(move |options| {
            let entered = entered.clone();
            Box::pin(async move {
                entered.notify_one();
                std::future::pending::<()>().await;
                drop(options.session_manager);
                Err("unreachable".into())
            })
        })
    };
    let host = AgentSessionRuntime::new(initial, factory);
    let pending = {
        let host = host.clone();
        let path = path.clone();
        tokio::spawn(async move { host.switch_session(path, None).await })
    };
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), host.dispose())
        .await
        .unwrap()
        .unwrap();
    assert!(pending.await.unwrap().is_err());
    assert!(host.is_disposed());
    assert!(old.is_closed());
    assert!(SessionManager::open(path, None, None).is_ok());
}

#[tokio::test]
async fn runtime_before_and_shutdown_callbacks_reject_structural_self_wait() {
    use pi_runtime::*;
    let services = runtime_services().await;
    let holder = Arc::new(std::sync::Mutex::new(None::<AgentSessionRuntime>));
    let checks = Arc::new(AtomicUsize::new(0));
    let extension = {
        let (holder, checks) = (holder.clone(), checks.clone());
        InlineExtension::new("<runtime-reentrant>", move |api| {
            for event in ["session_before_switch", "session_shutdown"] {
                let (holder, checks) = (holder.clone(), checks.clone());
                api.on_lifecycle(event, move |_, _| {
                    let (host, checks) = (
                        holder.lock().unwrap().as_ref().unwrap().clone(),
                        checks.clone(),
                    );
                    async move {
                        assert!(
                            host.new_session(None)
                                .await
                                .unwrap_err()
                                .contains("Recursive")
                        );
                        assert!(host.dispose().await.unwrap_err().contains("Recursive"));
                        checks.fetch_add(1, Ordering::SeqCst);
                        Ok(pi_session::SessionBeforeEventOutput::None)
                    }
                });
            }
            Ok(())
        })
    };
    let initial = runtime_result(
        services.clone(),
        runtime_manager(&services),
        vec![Box::new(extension)],
    );
    let host = AgentSessionRuntime::new(initial, runtime_factory(services));
    *holder.lock().unwrap() = Some(host.clone());
    tokio::time::timeout(Duration::from_secs(3), host.new_session(None))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(checks.load(Ordering::SeqCst), 2);
    holder.lock().unwrap().take();
    host.dispose().await.unwrap();
}

#[tokio::test]
async fn runtime_callback_failure_keeps_committed_target_active() {
    use pi_runtime::*;
    let services = runtime_services().await;
    let initial = runtime_result(services.clone(), runtime_manager(&services), vec![]);
    let old = initial.session.clone();
    let host = AgentSessionRuntime::new(initial, runtime_factory(services));
    let result = host
        .new_session(Some(NewSessionRuntimeOptions {
            with_session: Some(Arc::new(|_| {
                Box::pin(async { Err("host callback failure".into()) })
            })),
            ..Default::default()
        }))
        .await;
    assert_eq!(result.unwrap_err(), "host callback failure");
    assert!(old.is_closed());
    assert!(!host.is_failed());
    host.session()
        .set_session_name("target remains active")
        .unwrap();
    host.dispose().await.unwrap();
}

#[tokio::test]
async fn runtime_import_preflight_cleanup_preserves_existing_collision() {
    use pi_runtime::*;
    let services = runtime_services().await;
    let initial = runtime_result(services.clone(), runtime_manager(&services), vec![]);
    let old = initial.session.clone();
    let source = services.cwd.join("incoming.jsonl");
    std::fs::write(&source, b"invalid session header\n").unwrap();
    let collision = old.with_session_manager(|sm| sm.get_session_dir().join("incoming.jsonl"));
    std::fs::write(&collision, b"keep existing file").unwrap();
    let host = AgentSessionRuntime::new(initial, runtime_factory(services));
    assert!(host.import_from_jsonl(source, None).await.is_err());
    assert_eq!(std::fs::read(&collision).unwrap(), b"keep existing file");
    assert!(!collision.with_file_name("incoming-1.jsonl").exists());
    assert!(!old.is_closing_or_closed());
    host.dispose().await.unwrap();
}

#[tokio::test]
async fn runtime_persisted_fork_derives_only_after_shutdown_releases_old_store() {
    use pi_runtime::*;
    let services = runtime_services().await;
    let sm = runtime_manager(&services);
    let dir = sm.get_session_dir().to_path_buf();
    let entry = sm.get_leaf_id().unwrap().to_owned();
    let original = sm.get_session_file().unwrap().to_path_buf();
    let extension = InlineExtension::new("<fork-order>", move |api| {
        let dir = dir.clone();
        api.on_lifecycle("session_shutdown", move |_, _| {
            let count = std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
                .count();
            async move {
                assert_eq!(
                    count, 1,
                    "fork file must be derived after old session shutdown"
                );
                Ok(pi_session::SessionBeforeEventOutput::None)
            }
        });
        Ok(())
    });
    let initial = runtime_result(services.clone(), sm, vec![Box::new(extension)]);
    let old = initial.session.clone();
    let host = AgentSessionRuntime::new(initial, runtime_factory(services));
    host.fork(
        &entry,
        Some(ForkSessionRuntimeOptions {
            position: Some(ForkPosition::At),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert!(old.is_closed());
    assert_ne!(host.session().session_file().unwrap(), original);
    assert!(SessionManager::open(original, None, None).is_ok());
    host.dispose().await.unwrap();
}

#[tokio::test]
async fn agent_spawned_tool_hooks_and_execute_reject_own_shutdown_wait() {
    use pi_ai::faux::*;
    let holder = Arc::new(std::sync::Mutex::new(None::<AgentSession>));
    let checks = Arc::new(AtomicUsize::new(0));
    let (session, _) = persistent_session(|opts| {
        let faux = register_faux_provider(&opts.models);
        opts.model = Some(faux.model.clone());
        faux.set_responses(vec![
            faux_assistant_message(
                vec![faux_tool_call("self_wait", Default::default(), None)],
                Some(pi_ai::StopReason::ToolUse),
            )
            .into(),
            faux_text_message("finished").into(),
        ]);
        let (tool_holder, tool_checks) = (holder.clone(), checks.clone());
        opts.base_tools.push(pi_session::ToolDefinition::new(
            "self_wait",
            "Self wait",
            "verify own context",
            serde_json::json!({"type":"object","properties":{}}),
            Arc::new(move |_, _, _, _, _| {
                let (session, checks) = (
                    tool_holder.lock().unwrap().as_ref().unwrap().clone(),
                    tool_checks.clone(),
                );
                Box::pin(async move {
                    assert!(
                        session
                            .shutdown()
                            .await
                            .unwrap_err()
                            .contains("own session callback")
                    );
                    checks.fetch_add(1, Ordering::SeqCst);
                    Ok(pi_agent::AgentToolResult::text("tool returned"))
                })
            }),
        ));
        opts.initial_active_tool_names = Some(vec!["self_wait".into()]);
        let (holder, checks) = (holder.clone(), checks.clone());
        opts.extensions.push(Box::new(InlineExtension::new(
            "<self-wait-hook>",
            move |api| {
                let (call_holder, call_checks) = (holder.clone(), checks.clone());
                api.on_tool_call(move |_, _| {
                    let (session, checks) = (
                        call_holder.lock().unwrap().as_ref().unwrap().clone(),
                        call_checks.clone(),
                    );
                    async move {
                        assert!(
                            session
                                .shutdown()
                                .await
                                .unwrap_err()
                                .contains("own session callback")
                        );
                        checks.fetch_add(1, Ordering::SeqCst);
                        Ok(None)
                    }
                });
                let (result_holder, result_checks) = (holder.clone(), checks.clone());
                api.on_tool_result(move |_, _| {
                    let (session, checks) = (
                        result_holder.lock().unwrap().as_ref().unwrap().clone(),
                        result_checks.clone(),
                    );
                    async move {
                        assert!(
                            session
                                .shutdown()
                                .await
                                .unwrap_err()
                                .contains("own session callback")
                        );
                        checks.fetch_add(1, Ordering::SeqCst);
                        Ok(None)
                    }
                });
                Ok(())
            },
        )));
    });
    *holder.lock().unwrap() = Some(session.clone());
    tokio::time::timeout(Duration::from_secs(3), session.prompt("run", None))
        .await
        .expect("spawned callbacks must not await their own shutdown")
        .unwrap();
    assert_eq!(checks.load(Ordering::SeqCst), 3);
    holder.lock().unwrap().take();
    session.shutdown().await.unwrap();
}

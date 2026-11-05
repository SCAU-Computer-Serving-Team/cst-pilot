use super::persistent_session;
use pi_session::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

#[tokio::test]
async fn conditional_usage_commit_rechecks_after_waiting_for_admission() {
    let (session, _) = persistent_session(|_| {});
    let (entered, waiting) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let held = session.clone();
    let holder = tokio::task::spawn_blocking(move || {
        held.with_session_manager_mut(|_| {
            entered.send(()).unwrap();
            released.recv().unwrap();
            Ok(())
        })
    });
    tokio::task::spawn_blocking(move || waiting.recv().unwrap())
        .await
        .unwrap();
    let current = Arc::new(AtomicBool::new(true));
    let (check, clone) = (current.clone(), session.clone());
    let append = tokio::task::spawn_blocking(move || {
        clone.with_session_manager_mut_if(
            || check.load(Ordering::SeqCst),
            |_| -> Result<(), pi_session::SessionStoreError> {
                panic!("stale usage must not commit")
            },
        )
    });
    current.store(false, Ordering::SeqCst);
    release.send(()).unwrap();
    holder.await.unwrap().unwrap();
    assert_eq!(append.await.unwrap().unwrap(), None);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn panicking_close_callbacks_still_share_failure_and_release_stores() {
    for closing in [false, true] {
        let (session, path) = persistent_session(|opts| {
            if closing {
                opts.on_session_closing =
                    Some(Arc::new(|| panic!("injected close callback failure")));
            }
        });
        if !closing {
            session.set_before_session_invalidate(Some(Arc::new(|| {
                panic!("injected invalidate callback failure")
            })));
        }
        let (a, b) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(session.shutdown(), session.shutdown())
        })
        .await
        .expect("callback panic cannot leave Closing forever");
        assert_eq!(a, b);
        assert!(a.unwrap_err().contains("callback panicked"));
        assert!(session.is_closed());
        assert!(SessionManager::open(path, None, None).is_ok());
    }
}

#[test]
fn dispose_without_a_tokio_runtime_completes_the_same_close_lifecycle() {
    let called = Arc::new(AtomicBool::new(false));
    let (session, path) = persistent_session(|opts| {
        let called = called.clone();
        opts.on_session_closing = Some(Arc::new(move || {
            called.store(true, Ordering::SeqCst);
        }));
    });
    session.dispose();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime
        .block_on(async { tokio::time::timeout(Duration::from_secs(3), session.shutdown()).await })
        .unwrap()
        .unwrap();
    assert!(called.load(Ordering::SeqCst));
    assert!(session.is_closed());
    assert!(SessionManager::open(path, None, None).is_ok());
}

#[tokio::test]
async fn storage_callbacks_reject_reentry_and_can_schedule_dispose_without_deadlock() {
    let (session, path) = persistent_session(|_| {});
    session
        .with_session_manager_mut(|_| {
            assert!(
                session
                    .with_session_manager_mut(|_| Ok(()))
                    .unwrap_err()
                    .to_string()
                    .contains("reenter")
            );
            assert!(
                session
                    .with_execution_log_mut(|_| Ok(()))
                    .unwrap_err()
                    .to_string()
                    .contains("reenter")
            );
            assert!(
                session
                    .set_thinking_level(pi_ai::ModelThinkingLevel::Off)
                    .unwrap_err()
                    .contains("reenter")
            );
            Ok(())
        })
        .unwrap();
    // Same-store reads fail the enclosing callback instead of poisoning its mutex.
    let result = session.with_session_manager_mut(|_| {
        let _ = session.session_id();
        Ok(())
    });
    assert!(result.unwrap_err().to_string().contains("callback failed"));
    assert!(!session.session_id().is_empty());
    session
        .with_execution_log_mut(|_| {
            session.dispose();
            Ok(())
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), session.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(SessionManager::open(path, None, None).is_ok());
}

#[tokio::test]
async fn input_hook_is_cancelled_on_shutdown_even_after_prompt_waiter_is_dropped() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    struct Guard(Arc<AtomicBool>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let (session, path) = persistent_session(|opts| {
        let (entered, dropped) = (entered.clone(), dropped.clone());
        opts.extensions.push(Box::new(InlineExtension::new(
            "<input-pending>",
            move |api| {
                let (entered, dropped) = (entered.clone(), dropped.clone());
                api.on_input(move |_, _| {
                    let (entered, dropped) = (entered.clone(), dropped.clone());
                    async move {
                        let _guard = Guard(dropped);
                        entered.notify_one();
                        std::future::pending().await
                    }
                });
                Ok(())
            },
        )));
    });
    let copy = session.clone();
    let task = tokio::spawn(async move { copy.prompt("input", None).await });
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    task.abort();
    let _ = task.await;
    assert!(!session.is_idle());
    tokio::time::timeout(Duration::from_secs(3), session.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(dropped.load(Ordering::SeqCst));
    assert!(SessionManager::open(path, None, None).is_ok());
}

#[tokio::test]
async fn dropped_sdk_construction_closes_partial_session_and_releases_its_lock() {
    use pi_runtime::*;
    let services = super::runtime_services().await;
    let mut sm = SessionManager::create(
        services.cwd.to_string_lossy(),
        services.cwd.join("sessions"),
        None,
    )
    .unwrap();
    sm.append_message(pi_agent::AgentMessage::User(pi_ai::UserMessage::new(
        "persisted",
    )))
    .unwrap();
    let path = sm.get_session_file().unwrap().to_path_buf();
    let entered = Arc::new(tokio::sync::Notify::new());
    let shutdowns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (e, c) = (entered.clone(), shutdowns.clone());
    let loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: services.cwd.clone(),
        agent_dir: services.agent_dir.clone(),
        no_skills: true,
        no_context_files: true,
        no_prompt_templates: true,
        extension_factories: vec![Arc::new(move || {
            let (e, c) = (e.clone(), c.clone());
            Ok(Box::new(InlineExtension::new(
                "<partial-sdk>",
                move |api| {
                    let e = e.clone();
                    api.on_lifecycle("session_start", move |_, _| {
                        let e = e.clone();
                        async move {
                            e.notify_one();
                            std::future::pending().await
                        }
                    });
                    let c = c.clone();
                    api.on_lifecycle("session_shutdown", move |_, _| {
                        c.fetch_add(1, Ordering::SeqCst);
                        async { Ok(SessionBeforeEventOutput::None) }
                    });
                    Ok(())
                },
            )))
        })],
        ..Default::default()
    }));
    let task = tokio::spawn(async move {
        create_agent_session(CreateAgentSessionOptions {
            cwd: Some(services.cwd),
            agent_dir: Some(services.agent_dir),
            model_runtime: Some(services.model_runtime),
            settings_manager: Some(services.settings_manager),
            resource_loader: Some(loader),
            session_manager: Some(sm),
            ..Default::default()
        })
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    task.abort();
    let _ = task.await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if SessionManager::open_existing(&path, None, None).is_ok() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("partially constructed SDK session must release its persistent lock");
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn model_selection_append_failure_does_not_publish_new_model() {
    let (session, _) = persistent_session(|_| {});
    let before = session.model();
    let generation = session.selection_generation();
    let fault = session.with_session_manager(|sm| sm.fault_injection().unwrap());
    fault.lock().unwrap().fail_append_after_bytes = Some(1);
    let mut next = pi_ai::faux::create_faux_provider().model;
    next.id = "different".into();
    let error = session.set_model(next).await.unwrap_err();
    assert!(error.contains("persist model selection"));
    assert_eq!(session.model(), before);
    assert_eq!(session.selection_generation(), generation);
    assert!(session.fatal_runtime_error().is_some());
    fault.lock().unwrap().fail_append_after_bytes = None;
    assert!(session.shutdown().await.is_err());
}

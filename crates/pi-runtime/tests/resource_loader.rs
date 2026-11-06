#[path = "resource_loader/algorithms.rs"]
mod algorithms;
#[path = "resource_loader/contracts.rs"]
mod contracts;

use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use pi_runtime::{DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader};
use pi_session::{ExtensionFactory, ExtensionInitError, InlineExtension};

fn root() -> PathBuf {
    let root = std::env::temp_dir()
        .join("cst-pilot")
        .join(format!("resources-{}", pi_ai::uuid_v7()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[tokio::test]
async fn shared_services_use_independent_catalogs_and_discovered_resources() {
    use pi_runtime::{
        CreateAgentSessionOptions, CreateAgentSessionServicesOptions, CreateModelRuntimeOptions,
        ModelRuntime, create_agent_session, create_agent_session_services,
    };
    use pi_session::{ResourcesDiscoverResult, SessionManager};
    let root = root();
    let builds = Arc::new(AtomicUsize::new(0));
    let discovers = Arc::new(AtomicUsize::new(0));
    let registrations = Arc::new(AtomicUsize::new(0));
    let fail = Arc::new(AtomicBool::new(false));
    let factory: ExtensionFactory = {
        let (root, builds, discovers, registrations, fail) = (
            root.clone(),
            builds.clone(),
            discovers.clone(),
            registrations.clone(),
            fail.clone(),
        );
        Arc::new(move || {
            let n = builds.fetch_add(1, Ordering::SeqCst) + 1;
            if fail.load(Ordering::SeqCst) {
                return Err(ExtensionInitError {
                    extension_path: "<catalog>".into(),
                    message: "factory failed".into(),
                });
            }
            let skill_dir = root.join(format!("skill-{n}"));
            std::fs::create_dir_all(&skill_dir).unwrap();
            std::fs::write(
                skill_dir.join("SKILL.md"),
                format!("---\nname: skill-{n}\ndescription: test resource\n---\nresource {n}\n"),
            )
            .unwrap();
            let path = skill_dir.to_string_lossy().into_owned();
            let faux = pi_ai::faux::create_faux_provider();
            let mut model = faux.model.clone();
            model.id = format!("model-{n}");
            let mut provider_options = faux.provider.to_create_options();
            provider_options.models = vec![pi_ai::AnyModel::Chat(model)];
            let provider = pi_ai::create_provider(provider_options).unwrap();
            let (discovers, registrations) = (discovers.clone(), registrations.clone());
            Ok(Box::new(InlineExtension::new("<catalog>", move |api| {
                registrations.fetch_add(1, Ordering::SeqCst);
                api.register_provider(provider.clone());
                let (discovers, path) = (discovers.clone(), path.clone());
                api.on_resources_discover(move |_, _| {
                    discovers.fetch_add(1, Ordering::SeqCst);
                    let path = path.clone();
                    async move {
                        Ok(Some(ResourcesDiscoverResult {
                            skill_paths: vec![path],
                            ..Default::default()
                        }))
                    }
                });
                Ok(())
            })))
        })
    };
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        include_builtins: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await;
    let services = create_agent_session_services(CreateAgentSessionServicesOptions {
        cwd: root.clone(),
        agent_dir: Some(root.join("agent")),
        model_runtime: Some(runtime),
        resource_loader_options: Some(DefaultResourceLoaderOptions {
            extension_factories: vec![factory],
            no_context_files: true,
            no_prompt_templates: true,
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;
    let make = || CreateAgentSessionOptions {
        cwd: Some(root.clone()),
        agent_dir: Some(root.join("agent")),
        model_runtime: Some(services.model_runtime.clone()),
        resource_loader: Some(services.resource_loader.clone()),
        settings_manager: Some(services.settings_manager.clone()),
        session_manager: Some(
            SessionManager::in_memory(root.to_string_lossy(), None, None).unwrap(),
        ),
        ..Default::default()
    };
    let first = create_agent_session(make()).await.unwrap();
    let second = create_agent_session(make()).await.unwrap();
    assert_eq!(builds.load(Ordering::SeqCst), 2);
    assert_eq!(discovers.load(Ordering::SeqCst), 2);
    assert_eq!(registrations.load(Ordering::SeqCst), 2);
    assert!(Arc::ptr_eq(
        first.session.models(),
        first.model_runtime.models()
    ));
    assert!(!Arc::ptr_eq(
        first.session.models(),
        second.session.models()
    ));
    let provider = pi_ai::faux::create_faux_provider().model.provider;
    assert!(
        first
            .model_runtime
            .get_model(&provider, "model-1")
            .is_some()
    );
    assert!(
        first
            .model_runtime
            .get_model(&provider, "model-2")
            .is_none()
    );
    assert!(
        second
            .model_runtime
            .get_model(&provider, "model-2")
            .is_some()
    );
    assert_eq!(first.session.skills()[0].name, "skill-1");
    assert_eq!(second.session.skills()[0].name, "skill-2");
    first.session.reload_resources().await.unwrap();
    assert!(
        first
            .model_runtime
            .get_model(&provider, "model-1")
            .is_none()
    );
    assert!(
        first
            .model_runtime
            .get_model(&provider, "model-3")
            .is_some()
    );
    assert!(
        second
            .model_runtime
            .get_model(&provider, "model-2")
            .is_some()
    );
    assert_eq!(first.session.skills()[0].name, "skill-3");
    assert_eq!(second.session.skills()[0].name, "skill-2");
    fail.store(true, Ordering::SeqCst);
    assert!(
        first
            .session
            .reload_resources()
            .await
            .unwrap_err()
            .contains("factory failed")
    );
    assert!(
        first
            .model_runtime
            .get_model(&provider, "model-3")
            .is_some()
    );
    assert_eq!(first.session.skills()[0].name, "skill-3");
    first.session.shutdown().await.unwrap();
    assert!(
        second
            .model_runtime
            .get_model(&provider, "model-2")
            .is_some()
    );
    second.session.shutdown().await.unwrap();
    assert!(
        second
            .model_runtime
            .get_model(&provider, "model-2")
            .is_none()
    );
    assert_eq!(builds.load(Ordering::SeqCst), 4);
    assert_eq!(discovers.load(Ordering::SeqCst), 3);
    assert_eq!(registrations.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn cancelled_reload_does_not_install_resources_after_close() {
    use pi_session::{AgentSession, AgentSessionOptions, SessionManager};
    use std::time::Duration;
    use tokio::sync::Notify;
    let root = root();
    let entered = Arc::new(Notify::new());
    let sm = SessionManager::in_memory(root.to_string_lossy(), None, None).unwrap();
    let mut options = AgentSessionOptions::new_unconfigured(
        root.to_string_lossy(),
        Arc::new(pi_ai::Models::default()),
        sm,
    );
    options.on_reload = Some({
        let entered = entered.clone();
        Arc::new(move || {
            let entered = entered.clone();
            Box::pin(async move {
                entered.notify_one();
                std::future::pending().await
            })
        })
    });
    let session = AgentSession::new(options).unwrap();
    let clone = session.clone();
    let reload = tokio::spawn(async move { clone.reload_resources().await });
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    reload.abort();
    let _ = reload.await;
    tokio::time::timeout(Duration::from_secs(3), session.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(session.is_closed());
    assert!(
        session
            .reload_resources()
            .await
            .unwrap_err()
            .contains("closed")
    );
}

#[tokio::test]
async fn registration_failure_does_not_reinitialize_factories_or_replace_state() {
    let root = root();
    let registrations = Arc::new(AtomicUsize::new(0));
    let builds = Arc::new(AtomicUsize::new(0));
    let fail = Arc::new(AtomicBool::new(false));
    let factory: ExtensionFactory = {
        let (registrations, builds, fail) = (registrations.clone(), builds.clone(), fail.clone());
        Arc::new(move || {
            builds.fetch_add(1, Ordering::SeqCst);
            let (registrations, fail) = (registrations.clone(), fail.clone());
            Ok(Box::new(InlineExtension::new(
                "<register-test>",
                move |_| {
                    registrations.fetch_add(1, Ordering::SeqCst);
                    if fail.load(Ordering::SeqCst) {
                        Err(ExtensionInitError {
                            extension_path: "<register-test>".into(),
                            message: "registration failed".into(),
                        })
                    } else {
                        Ok(())
                    }
                },
            )))
        })
    };
    let loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: root.clone(),
        agent_dir: root.clone(),
        extension_factories: vec![factory],
        no_skills: true,
        no_prompt_templates: true,
        no_context_files: true,
        ..Default::default()
    });
    loader.reload().await.unwrap();
    fail.store(true, Ordering::SeqCst);
    assert!(
        loader
            .load_session_resources(true)
            .await
            .err()
            .unwrap()
            .contains("registration failed")
    );
    assert_eq!(builds.load(Ordering::SeqCst), 2);
    assert_eq!(registrations.load(Ordering::SeqCst), 2);
    assert_eq!(
        loader.get_extensions().extension_paths,
        vec!["<register-test>"]
    );
}

#[tokio::test]
async fn failed_factory_reload_keeps_previous_resources_and_runner() {
    let root = root();
    let fail = Arc::new(AtomicBool::new(false));
    let builds = Arc::new(AtomicUsize::new(0));
    let factory: ExtensionFactory = {
        let (fail, builds) = (fail.clone(), builds.clone());
        Arc::new(move || {
            builds.fetch_add(1, Ordering::SeqCst);
            if fail.load(Ordering::SeqCst) {
                return Err(ExtensionInitError {
                    extension_path: "<test>".into(),
                    message: "injected failure".into(),
                });
            }
            Ok(Box::new(InlineExtension::new("<test>", |_| Ok(()))))
        })
    };
    std::fs::write(root.join("SYSTEM.md"), "original prompt").unwrap();
    let loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: root.clone(),
        agent_dir: root.clone(),
        extension_factories: vec![factory],
        no_skills: true,
        no_prompt_templates: true,
        no_context_files: true,
        ..Default::default()
    });
    loader.reload().await.unwrap();
    fail.store(true, Ordering::SeqCst);
    std::fs::write(root.join("SYSTEM.md"), "replacement prompt").unwrap();
    assert!(
        loader
            .reload()
            .await
            .unwrap_err()
            .contains("injected failure")
    );
    assert_eq!(
        loader.get_system_prompt().as_deref(),
        Some("original prompt")
    );
    assert_eq!(loader.get_extensions().extension_paths, vec!["<test>"]);
    assert_eq!(builds.load(Ordering::SeqCst), 2);
}

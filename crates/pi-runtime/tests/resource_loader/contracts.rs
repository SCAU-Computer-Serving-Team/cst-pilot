use pi_runtime::*;
use pi_session::*;
use std::collections::BTreeMap;
use std::sync::Arc;

fn root() -> std::path::PathBuf {
    let p = std::path::PathBuf::from("E:/tmp/2026-11-04/argon-phase4-development/tests")
        .join(format!("resource-contracts-{}", pi_ai::uuid_v7()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[tokio::test]
async fn service_flags_reach_every_session_and_reload_preserve_explicit_false() {
    let root = root();
    let factory: ExtensionFactory = Arc::new(|| {
        Ok(Box::new(InlineExtension::new("<flags>", |api| {
            api.register_flag(
                "enabled",
                ExtensionFlagType::Boolean,
                Some(ExtensionFlagValue::Bool(true)),
                None,
            );
            api.register_flag(
                "label",
                ExtensionFlagType::String,
                Some(ExtensionFlagValue::String("default".into())),
                None,
            );
            Ok(())
        })))
    });
    let rt = ModelRuntime::create(CreateModelRuntimeOptions {
        include_builtins: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await;
    let services = create_agent_session_services(CreateAgentSessionServicesOptions {
        cwd: root.clone(),
        agent_dir: Some(root.join("agent")),
        model_runtime: Some(rt),
        extension_flag_values: Some(BTreeMap::from([
            ("enabled".into(), ExtensionFlagValue::Bool(false)),
            (
                "label".into(),
                ExtensionFlagValue::String("selected".into()),
            ),
        ])),
        resource_loader_options: Some(DefaultResourceLoaderOptions {
            extension_factories: vec![factory],
            no_skills: true,
            no_prompt_templates: true,
            no_context_files: true,
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;
    assert!(services.diagnostics.iter().all(|d| d.kind != "error"));
    for _ in 0..2 {
        let result = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(root.clone()),
            agent_dir: Some(root.join("agent")),
            model_runtime: Some(services.model_runtime.clone()),
            settings_manager: Some(services.settings_manager.clone()),
            resource_loader: Some(services.resource_loader.clone()),
            session_manager: Some(
                SessionManager::in_memory(root.to_string_lossy(), None, None).unwrap(),
            ),
            ..Default::default()
        })
        .await
        .unwrap();
        for _ in 0..2 {
            let values = result.session.extension_runner().get_flag_values();
            assert_eq!(
                values.get("enabled"),
                Some(&ExtensionFlagValue::Bool(false))
            );
            assert_eq!(
                values.get("label"),
                Some(&ExtensionFlagValue::String("selected".into()))
            );
            result.session.reload().await.unwrap();
        }
        result.session.shutdown().await.unwrap();
    }
}

struct ProjectExtension {
    path: String,
    base: std::path::PathBuf,
}
impl Extension for ProjectExtension {
    fn path(&self) -> &str {
        &self.path
    }
    fn source_info(&self) -> SourceInfo {
        let mut s = SourceInfo::synthetic(&self.path, "local");
        s.scope = "project".into();
        s.base_dir = Some(self.base.to_string_lossy().into_owned());
        s
    }
    fn register(&self, api: &mut ExtensionRegistrar) -> Result<(), ExtensionInitError> {
        api.on_resources_discover(|_, _| async {
            Ok(Some(ResourcesDiscoverResult {
                skill_paths: vec!["skill".into()],
                prompt_paths: vec!["review.md".into()],
                ..Default::default()
            }))
        });
        Ok(())
    }
}

#[tokio::test]
async fn discovered_resources_preserve_project_origin_even_outside_project_directory() {
    let root = root();
    let cwd = root.join("project");
    let external = root.join("outside");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(external.join("skill")).unwrap();
    std::fs::write(
        external.join("skill/SKILL.md"),
        "---\nname: discovered\ndescription: text resource\n---\nbody",
    )
    .unwrap();
    std::fs::write(external.join("review.md"), "review text").unwrap();
    let origin = cwd
        .join(".pi/extensions/static")
        .to_string_lossy()
        .into_owned();
    let factory: ExtensionFactory = {
        let origin = origin.clone();
        Arc::new(move || {
            Ok(Box::new(ProjectExtension {
                path: origin.clone(),
                base: external.clone(),
            }))
        })
    };
    let loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: cwd.clone(),
        agent_dir: root.join("agent"),
        settings_manager: Some(SettingsManager::create_with_trust(
            &cwd,
            root.join("agent"),
            false,
        )),
        extension_factories: vec![factory],
        ..Default::default()
    });
    loader.reload().await.unwrap();
    let resources = loader.load_session_resources(false).await.unwrap();
    assert_eq!(resources.skills.skills.len(), 1);
    assert_eq!(resources.prompts.templates.len(), 1);
    let resolver = ConfigValueResolver::new(&cwd, root.join("agent"), false);
    for info in [
        &resources.skills.skills[0].source_info,
        &resources.prompts.templates[0].source_info,
    ] {
        assert_eq!(info.scope, "project");
        assert_eq!(info.path, origin);
        let error = resolver
            .resolve_or_throw(
                "!echo not-allowed",
                "resource credential",
                None,
                ConfigValueSource::from_source_info(info),
                None,
            )
            .unwrap_err();
        assert!(error.contains("untrusted project"));
        assert!(!error.contains("not-allowed"));
    }
}

#[tokio::test]
async fn no_resource_defaults_skip_loading_but_keep_explicit_paths_and_prompt_overrides() {
    let root = root();
    let skill_dir = root.join("explicit");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: explicit\ndescription: explicit resource\n---\nbody",
    )
    .unwrap();
    std::fs::create_dir_all(root.join(".pi/skills/default")).unwrap();
    std::fs::write(
        root.join(".pi/skills/default/SKILL.md"),
        "---\nname: default\ndescription: not loaded\n---\nbody",
    )
    .unwrap();
    std::fs::write(root.join("AGENTS.md"), "not loaded").unwrap();
    std::fs::write(root.join(".pi/SYSTEM.md"), "\u{feff}custom system").unwrap();
    std::fs::write(root.join(".pi/APPEND_SYSTEM.md"), "\u{feff}append system").unwrap();
    let loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: root.clone(),
        agent_dir: root.join("agent"),
        no_skills: true,
        no_prompt_templates: true,
        no_context_files: true,
        no_extensions: true,
        additional_skill_paths: vec![skill_dir],
        ..Default::default()
    });
    loader.reload().await.unwrap();
    let resources = loader.load_session_resources(false).await.unwrap();
    assert_eq!(
        resources
            .skills
            .skills
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["explicit"]
    );
    assert!(resources.agents_files.is_empty());
    assert!(resources.prompts.templates.is_empty());
    assert_eq!(resources.system_prompt.as_deref(), Some("custom system"));
    assert_eq!(resources.append_system_prompt, ["append system"]);
}

#[tokio::test]
async fn explicit_resource_extension_is_persistent_and_override_callbacks_can_read_loader() {
    let root = root();
    let skill_dir = root.join("extra");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: extra\ndescription: extra skill\n---\nbody",
    )
    .unwrap();
    let weak = Arc::new(std::sync::Mutex::new(
        None::<std::sync::Weak<DefaultResourceLoader>>,
    ));
    let weak_copy = weak.clone();
    let loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: root.clone(),
        agent_dir: root.join("agent"),
        skills_override: Some(Arc::new(move |skills| {
            if let Some(loader) = weak_copy
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|weak| weak.upgrade())
            {
                let _ = loader.get_skills();
            }
            skills
        })),
        ..Default::default()
    }));
    *weak.lock().unwrap() = Some(Arc::downgrade(&loader));
    loader.reload().await.unwrap();
    let mut source = SourceInfo::synthetic("project-extension", "extension");
    source.scope = "project".into();
    loader
        .extend_resources(ResourceExtensionPaths {
            skill_paths: vec![(skill_dir, source)],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(loader.get_skills().skills.len(), 1);
    loader.reload().await.unwrap();
    assert_eq!(loader.get_skills().skills[0].source_info.scope, "project");
}

#[tokio::test]
async fn invalid_extension_flag_types_return_redacted_diagnostics() {
    let root = root();
    let services = create_agent_session_services(CreateAgentSessionServicesOptions {
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
        extension_flag_values: Some(BTreeMap::from([(
            "enabled".into(),
            ExtensionFlagValue::String("mock-secret-value".into()),
        )])),
        resource_loader_options: Some(DefaultResourceLoaderOptions {
            extension_factories: vec![Arc::new(|| {
                Ok(Box::new(InlineExtension::new("<flag>", |api| {
                    api.register_flag("enabled", ExtensionFlagType::Boolean, None, None);
                    Ok(())
                })))
            })],
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;
    assert!(
        services
            .diagnostics
            .iter()
            .any(|d| d.kind == "error" && d.message.contains("requires a boolean"))
    );
    assert!(!format!("{:?}", services.diagnostics).contains("mock-secret-value"));
}

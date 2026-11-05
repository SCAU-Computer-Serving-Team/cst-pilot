use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use pi_agent::BoxFuture;
use pi_session::{
    Extension, ExtensionFactory, ExtensionFlagValue, ExtensionInitError, ExtensionRunner,
    PromptContextFile, SourceInfo,
};

use crate::auth_storage::strip_utf8_bom;
use crate::context_files::{LoadContextFilesOptions, load_project_context_files};
use crate::prompt_templates::{
    LoadPromptTemplatesOptions, LoadPromptTemplatesResult, PromptTemplate, load_prompt_templates,
};
use crate::settings_manager::{CONFIG_DIR_NAME, SettingsManager};
use crate::skills::{LoadSkillsOptions, LoadSkillsResult, ResourceDiagnostic, Skill, load_skills};

#[derive(Clone)]
pub struct BuiltinExtensionDescriptor {
    pub name: String,
    pub factory: ExtensionFactory,
}

impl BuiltinExtensionDescriptor {
    pub fn new(name: impl Into<String>, factory: ExtensionFactory) -> Self {
        Self {
            name: name.into(),
            factory,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ResourceExtensionPaths {
    pub skill_paths: Vec<(PathBuf, SourceInfo)>,
    pub prompt_paths: Vec<(PathBuf, SourceInfo)>,
    pub theme_paths: Vec<(PathBuf, SourceInfo)>,
}

#[derive(Clone)]
pub struct LoadExtensionsResult {
    pub runner: ExtensionRunner,
    pub extension_paths: Vec<String>,
    pub errors: Vec<ExtensionInitError>,
    pub warnings: Vec<ResourceDiagnostic>,
}

#[derive(Clone)]
pub struct SessionResources {
    pub extensions: LoadExtensionsResult,
    pub skills: LoadSkillsResult,
    pub prompts: LoadPromptTemplatesResult,
    pub agents_files: Vec<PromptContextFile>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Vec<String>,
}

pub type SkillsOverrideFn = Arc<dyn Fn(LoadSkillsResult) -> LoadSkillsResult + Send + Sync>;
pub type PromptsOverrideFn =
    Arc<dyn Fn(LoadPromptTemplatesResult) -> LoadPromptTemplatesResult + Send + Sync>;
pub type AgentsFilesOverrideFn =
    Arc<dyn Fn(Vec<PromptContextFile>) -> Vec<PromptContextFile> + Send + Sync>;
pub type SystemPromptOverrideFn = Arc<dyn Fn(Option<String>) -> Option<String> + Send + Sync>;
pub type AppendSystemPromptOverrideFn = Arc<dyn Fn(Vec<String>) -> Vec<String> + Send + Sync>;

pub trait ResourceLoader: Send + Sync {
    fn get_extensions(&self) -> LoadExtensionsResult;
    fn acquire_session_extension_runner(&self) -> Result<ExtensionRunner, ExtensionInitError>;
    fn get_skills(&self) -> LoadSkillsResult;
    fn get_prompts(&self) -> LoadPromptTemplatesResult;
    fn get_agents_files(&self) -> Vec<PromptContextFile>;
    fn get_system_prompt(&self) -> Option<String>;
    fn get_system_prompt_source(&self) -> Option<PathBuf>;
    fn get_append_system_prompt(&self) -> Vec<String>;
    fn get_append_system_prompt_sources(&self) -> Vec<PathBuf>;
    fn extend_resources(&self, paths: ResourceExtensionPaths) -> Result<(), String>;
    fn reload(&self) -> BoxFuture<'_, Result<(), String>>;

    /// Atomically acquire all resources and a uniquely owned, initialized runner.
    /// Implementations serialize reload, discovery, and snapshot transfer internally.
    fn load_session_resources(
        &self,
        reload: bool,
    ) -> BoxFuture<'_, Result<SessionResources, String>>;
}

#[derive(Clone, Default)]
pub struct DefaultResourceLoaderOptions {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub settings_manager: Option<SettingsManager>,
    pub additional_skill_paths: Vec<PathBuf>,
    pub additional_prompt_template_paths: Vec<PathBuf>,
    pub extension_factories: Vec<ExtensionFactory>,
    pub extension_flag_values: BTreeMap<String, ExtensionFlagValue>,
    pub builtin_extensions: Vec<BuiltinExtensionDescriptor>,
    pub disabled_builtin_extensions: Vec<String>,
    pub no_extensions: bool,
    pub no_skills: bool,
    pub no_prompt_templates: bool,
    pub no_context_files: bool,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Vec<String>,
    pub skills_override: Option<SkillsOverrideFn>,
    pub prompts_override: Option<PromptsOverrideFn>,
    pub agents_files_override: Option<AgentsFilesOverrideFn>,
    pub system_prompt_override: Option<SystemPromptOverrideFn>,
    pub append_system_prompt_override: Option<AppendSystemPromptOverrideFn>,
}

#[derive(Clone)]
struct DefaultResourceLoaderState {
    extensions_result: LoadExtensionsResult,
    unclaimed_runner: Option<ExtensionRunner>,
    skills_result: LoadSkillsResult,
    prompts_result: LoadPromptTemplatesResult,
    agents_files: Vec<PromptContextFile>,
    system_prompt: Option<String>,
    system_prompt_source: Option<PathBuf>,
    append_system_prompt: Vec<String>,
    append_system_prompt_sources: Vec<PathBuf>,
    dynamic_skill_paths: Vec<(PathBuf, SourceInfo)>,
    dynamic_prompt_paths: Vec<(PathBuf, SourceInfo)>,
    loaded: bool,
}

#[derive(Clone)]
pub struct DefaultResourceLoader {
    cwd: PathBuf,
    agent_dir: PathBuf,
    settings_manager: SettingsManager,
    additional_skill_paths: Vec<PathBuf>,
    additional_prompt_template_paths: Vec<PathBuf>,
    extension_factories: Vec<ExtensionFactory>,
    extension_flag_values: BTreeMap<String, ExtensionFlagValue>,
    builtin_extensions: Vec<BuiltinExtensionDescriptor>,
    disabled_builtin_extensions: BTreeSet<String>,
    no_extensions: bool,
    no_skills: bool,
    no_prompt_templates: bool,
    no_context_files: bool,
    system_prompt_input: Option<String>,
    append_system_prompt_input: Vec<String>,
    skills_override: Option<SkillsOverrideFn>,
    prompts_override: Option<PromptsOverrideFn>,
    agents_files_override: Option<AgentsFilesOverrideFn>,
    system_prompt_override: Option<SystemPromptOverrideFn>,
    append_system_prompt_override: Option<AppendSystemPromptOverrideFn>,
    state: Arc<RwLock<DefaultResourceLoaderState>>,
    external_resources: Arc<RwLock<ResourceExtensionPaths>>,
    reload_lock: Arc<tokio::sync::Mutex<()>>,
}

impl DefaultResourceLoader {
    pub fn new(options: DefaultResourceLoaderOptions) -> Self {
        let cwd = if options.cwd.as_os_str().is_empty() {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        } else {
            options.cwd
        };
        let agent_dir = if options.agent_dir.as_os_str().is_empty() {
            cwd.join(CONFIG_DIR_NAME)
        } else {
            options.agent_dir
        };
        let settings_manager = options
            .settings_manager
            .unwrap_or_else(|| SettingsManager::create(&cwd, &agent_dir));
        let cwd_str = cwd.to_string_lossy().into_owned();
        let empty_runner = ExtensionRunner::empty(&cwd_str);

        Self {
            cwd,
            agent_dir,
            settings_manager,
            additional_skill_paths: options.additional_skill_paths,
            additional_prompt_template_paths: options.additional_prompt_template_paths,
            extension_factories: options.extension_factories,
            extension_flag_values: options.extension_flag_values,
            builtin_extensions: options.builtin_extensions,
            disabled_builtin_extensions: options.disabled_builtin_extensions.into_iter().collect(),
            no_extensions: options.no_extensions,
            no_skills: options.no_skills,
            no_prompt_templates: options.no_prompt_templates,
            no_context_files: options.no_context_files,
            system_prompt_input: options.system_prompt,
            append_system_prompt_input: options.append_system_prompt,
            skills_override: options.skills_override,
            prompts_override: options.prompts_override,
            agents_files_override: options.agents_files_override,
            system_prompt_override: options.system_prompt_override,
            append_system_prompt_override: options.append_system_prompt_override,
            state: Arc::new(RwLock::new(DefaultResourceLoaderState {
                extensions_result: LoadExtensionsResult {
                    runner: empty_runner,
                    extension_paths: Vec::new(),
                    errors: Vec::new(),
                    warnings: Vec::new(),
                },
                unclaimed_runner: None,
                skills_result: LoadSkillsResult::default(),
                prompts_result: LoadPromptTemplatesResult::default(),
                agents_files: Vec::new(),
                system_prompt: None,
                system_prompt_source: None,
                append_system_prompt: Vec::new(),
                append_system_prompt_sources: Vec::new(),
                dynamic_skill_paths: Vec::new(),
                dynamic_prompt_paths: Vec::new(),
                loaded: false,
            })),
            external_resources: Arc::new(RwLock::new(ResourceExtensionPaths::default())),
            reload_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    fn instantiate_extensions(&self) -> (Vec<Box<dyn Extension>>, Vec<ExtensionInitError>) {
        if self.no_extensions {
            return (Vec::new(), Vec::new());
        }
        let mut instances: Vec<Box<dyn Extension>> = Vec::new();
        let mut errors: Vec<ExtensionInitError> = Vec::new();

        for builtin in &self.builtin_extensions {
            if self.disabled_builtin_extensions.contains(&builtin.name) {
                continue;
            }
            match (builtin.factory)() {
                Ok(ext) => instances.push(ext),
                Err(err) => errors.push(err),
            }
        }

        for factory in &self.extension_factories {
            match factory() {
                Ok(ext) => instances.push(ext),
                Err(err) => errors.push(err),
            }
        }

        (instances, errors)
    }

    fn build_runner_from_factories(&self) -> LoadExtensionsResult {
        let cwd_str = self.cwd.to_string_lossy().into_owned();
        let (candidates, mut errors) = self.instantiate_extensions();
        if candidates.is_empty() {
            let runner = ExtensionRunner::empty(&cwd_str);
            return LoadExtensionsResult {
                runner,
                extension_paths: Vec::new(),
                errors,
                warnings: Vec::new(),
            };
        }

        match ExtensionRunner::from_extensions(&cwd_str, candidates) {
            Ok(runner) => {
                let extension_paths = runner.get_extension_paths();
                let warnings = runner
                    .get_omitted_extension_warnings()
                    .into_iter()
                    .map(|w| ResourceDiagnostic {
                        kind: w.kind,
                        message: w.message,
                        path: Some(w.path),
                        collision: None,
                    })
                    .collect();
                LoadExtensionsResult {
                    runner,
                    extension_paths,
                    errors,
                    warnings,
                }
            }
            Err(err) => {
                errors.push(err);
                LoadExtensionsResult {
                    runner: ExtensionRunner::empty(&cwd_str),
                    extension_paths: Vec::new(),
                    errors,
                    warnings: Vec::new(),
                }
            }
        }
    }

    async fn prepare_resources(&self, reason: &str) -> Result<DefaultResourceLoaderState, String> {
        let mut ext_res = self.build_runner_from_factories();
        if let Some(err) = ext_res.errors.first() {
            return Err(err.to_string());
        }
        ext_res.warnings.extend(
            crate::sdk::apply_extension_flag_values(
                &ext_res.runner,
                Some(&self.extension_flag_values),
            )
            .into_iter()
            .map(|diag| ResourceDiagnostic {
                kind: diag.kind,
                message: diag.message,
                path: None,
                collision: None,
            }),
        );
        let cwd_str = self.cwd.to_string_lossy().into_owned();
        let discovered = if self.no_extensions {
            Vec::new()
        } else {
            ext_res
                .runner
                .emit_resources_discover(&cwd_str, reason)
                .await
        };

        let external = self
            .external_resources
            .read()
            .expect("external resources lock")
            .clone();
        let mut dyn_skills = external.skill_paths;
        let mut dyn_prompts = external.prompt_paths;
        for (source_info, res) in discovered {
            let base = source_info
                .base_dir
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(|| self.cwd.clone());
            for s in res.skill_paths {
                let p = Path::new(&s);
                let resolved = if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    base.join(p)
                };
                if !dyn_skills.iter().any(|(path, _)| path == &resolved) {
                    dyn_skills.push((resolved, source_info.clone()));
                }
            }
            for pr in res.prompt_paths {
                let p = Path::new(&pr);
                let resolved = if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    base.join(p)
                };
                if !dyn_prompts.iter().any(|(path, _)| path == &resolved) {
                    dyn_prompts.push((resolved, source_info.clone()));
                }
            }
        }

        let (
            skills_res,
            prompts_res,
            agents_files,
            sys_prompt,
            sys_source,
            append_prompts,
            append_sources,
        ) = self.reload_non_extension_resources(&dyn_skills, &dyn_prompts);

        Ok(DefaultResourceLoaderState {
            unclaimed_runner: Some(ext_res.runner.clone()),
            extensions_result: ext_res,
            skills_result: skills_res,
            prompts_result: prompts_res,
            agents_files,
            system_prompt: sys_prompt,
            system_prompt_source: sys_source,
            append_system_prompt: append_prompts,
            append_system_prompt_sources: append_sources,
            dynamic_skill_paths: dyn_skills,
            dynamic_prompt_paths: dyn_prompts,
            loaded: true,
        })
    }

    fn session_snapshot(st: &DefaultResourceLoaderState) -> SessionResources {
        SessionResources {
            extensions: st.extensions_result.clone(),
            skills: st.skills_result.clone(),
            prompts: st.prompts_result.clone(),
            agents_files: st.agents_files.clone(),
            system_prompt: st.system_prompt.clone(),
            append_system_prompt: st.append_system_prompt.clone(),
        }
    }

    fn resolve_prompt_input(&self, input: &str) -> (String, Option<PathBuf>) {
        let candidate = Path::new(input);
        let resolved = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.cwd.join(candidate)
        };
        if resolved.is_file()
            && let Ok(content) = fs::read_to_string(&resolved)
        {
            return (strip_utf8_bom(&content).to_owned(), Some(resolved));
        }
        (input.to_owned(), None)
    }

    fn discover_system_prompt_file(&self) -> Option<(String, PathBuf)> {
        let candidates = [
            self.cwd.join(CONFIG_DIR_NAME).join("SYSTEM.md"),
            self.agent_dir.join("SYSTEM.md"),
        ];
        for path in candidates {
            if path.is_file()
                && let Ok(raw) = fs::read_to_string(&path)
            {
                return Some((strip_utf8_bom(&raw).to_owned(), path));
            }
        }
        None
    }

    fn discover_append_system_prompt_file(&self) -> Option<(String, PathBuf)> {
        let candidates = [
            self.cwd.join(CONFIG_DIR_NAME).join("APPEND_SYSTEM.md"),
            self.agent_dir.join("APPEND_SYSTEM.md"),
        ];
        for path in candidates {
            if path.is_file()
                && let Ok(raw) = fs::read_to_string(&path)
            {
                return Some((strip_utf8_bom(&raw).to_owned(), path));
            }
        }
        None
    }

    fn reload_non_extension_resources(
        &self,
        dynamic_skill_paths: &[(PathBuf, SourceInfo)],
        dynamic_prompt_paths: &[(PathBuf, SourceInfo)],
    ) -> LoadedTextResources {
        // 1. Skills
        let mut skills_res = if self.no_skills && self.additional_skill_paths.is_empty() {
            LoadSkillsResult::default()
        } else {
            let mut skill_paths: Vec<PathBuf> = if self.no_skills {
                Vec::new()
            } else {
                self.settings_manager
                    .get_skill_paths()
                    .into_iter()
                    .map(PathBuf::from)
                    .chain(dynamic_skill_paths.iter().map(|(path, _)| path.clone()))
                    .collect()
            };
            skill_paths.extend(self.additional_skill_paths.iter().cloned());
            load_skills(&LoadSkillsOptions {
                cwd: self.cwd.clone(),
                agent_dir: self.agent_dir.clone(),
                skill_paths,
                include_defaults: !self.no_skills,
            })
        };
        for skill in &mut skills_res.skills {
            if skill.source_info.scope == "temporary"
                && let Some((_, info)) = dynamic_skill_paths
                    .iter()
                    .find(|(path, _)| resource_is_within(path, Path::new(&skill.file_path)))
            {
                skill.source_info = info.clone();
            }
        }
        if let Some(ref ov) = self.skills_override {
            skills_res = ov(skills_res);
        }

        // 2. Prompt Templates
        let mut prompts_res =
            if self.no_prompt_templates && self.additional_prompt_template_paths.is_empty() {
                LoadPromptTemplatesResult::default()
            } else {
                let mut prompt_paths: Vec<PathBuf> = if self.no_prompt_templates {
                    Vec::new()
                } else {
                    self.settings_manager
                        .get_prompt_template_paths()
                        .into_iter()
                        .map(PathBuf::from)
                        .chain(dynamic_prompt_paths.iter().map(|(path, _)| path.clone()))
                        .collect()
                };
                prompt_paths.extend(self.additional_prompt_template_paths.iter().cloned());
                load_prompt_templates(&LoadPromptTemplatesOptions {
                    cwd: self.cwd.clone(),
                    agent_dir: self.agent_dir.clone(),
                    prompt_paths,
                    include_defaults: !self.no_prompt_templates,
                })
            };
        for prompt in &mut prompts_res.templates {
            if prompt.source_info.scope == "temporary"
                && let Some((_, info)) = dynamic_prompt_paths
                    .iter()
                    .find(|(path, _)| resource_is_within(path, Path::new(&prompt.file_path)))
            {
                prompt.source_info = info.clone();
            }
        }
        if let Some(ref ov) = self.prompts_override {
            prompts_res = ov(prompts_res);
        }

        // 3. Context Files (AGENTS.md / CLAUDE.md)
        let mut agents_files = if self.no_context_files {
            Vec::new()
        } else {
            load_project_context_files(&LoadContextFilesOptions {
                cwd: self.cwd.clone(),
                agent_dir: self.agent_dir.clone(),
            })
        };
        if let Some(ref ov) = self.agents_files_override {
            agents_files = ov(agents_files);
        }

        // 4. System Prompt & Append System Prompt
        let (mut sys_prompt, sys_source) = match &self.system_prompt_input {
            Some(inp) => {
                let (content, src) = self.resolve_prompt_input(inp);
                (Some(content), src)
            }
            None => match self.discover_system_prompt_file() {
                Some((content, src)) => (Some(content), Some(src)),
                None => (None, None),
            },
        };
        if let Some(ref ov) = self.system_prompt_override {
            sys_prompt = ov(sys_prompt);
        }

        let mut append_prompts = Vec::new();
        let mut append_sources = Vec::new();
        if !self.append_system_prompt_input.is_empty() {
            for item in &self.append_system_prompt_input {
                let (content, src) = self.resolve_prompt_input(item);
                append_prompts.push(content);
                if let Some(s) = src {
                    append_sources.push(s);
                }
            }
        } else if let Some((content, src)) = self.discover_append_system_prompt_file() {
            append_prompts.push(content);
            append_sources.push(src);
        }
        if let Some(ref ov) = self.append_system_prompt_override {
            append_prompts = ov(append_prompts);
        }

        (
            skills_res,
            prompts_res,
            agents_files,
            sys_prompt,
            sys_source,
            append_prompts,
            append_sources,
        )
    }

    pub fn get_loaded_skills_vec(&self) -> Vec<Skill> {
        self.get_skills().skills
    }

    pub fn get_loaded_prompts_vec(&self) -> Vec<PromptTemplate> {
        self.get_prompts().templates
    }
}

type LoadedTextResources = (
    LoadSkillsResult,
    LoadPromptTemplatesResult,
    Vec<PromptContextFile>,
    Option<String>,
    Option<PathBuf>,
    Vec<String>,
    Vec<PathBuf>,
);

fn resource_is_within(base: &Path, path: &Path) -> bool {
    let base = crate::skills::resource_path_key(base);
    let path = crate::skills::resource_path_key(path);
    if path == base {
        return true;
    }
    let separator = if cfg!(windows) { '\\' } else { '/' };
    path.starts_with(&format!("{}{separator}", base.trim_end_matches(separator)))
}

impl ResourceLoader for DefaultResourceLoader {
    fn get_extensions(&self) -> LoadExtensionsResult {
        self.state
            .read()
            .expect("state lock")
            .extensions_result
            .clone()
    }

    fn acquire_session_extension_runner(&self) -> Result<ExtensionRunner, ExtensionInitError> {
        {
            let mut st = self.state.write().expect("state lock");
            if let Some(unclaimed) = st.unclaimed_runner.take() {
                return Ok(unclaimed);
            }
        }
        Err(ExtensionInitError {
            extension_path: "<resource-loader>".to_owned(),
            message: "Use load_session_resources to initialize a new session runner".to_owned(),
        })
    }

    fn get_skills(&self) -> LoadSkillsResult {
        self.state.read().expect("state lock").skills_result.clone()
    }

    fn get_prompts(&self) -> LoadPromptTemplatesResult {
        self.state
            .read()
            .expect("state lock")
            .prompts_result
            .clone()
    }

    fn get_agents_files(&self) -> Vec<PromptContextFile> {
        self.state.read().expect("state lock").agents_files.clone()
    }

    fn get_system_prompt(&self) -> Option<String> {
        self.state.read().expect("state lock").system_prompt.clone()
    }

    fn get_system_prompt_source(&self) -> Option<PathBuf> {
        self.state
            .read()
            .expect("state lock")
            .system_prompt_source
            .clone()
    }

    fn get_append_system_prompt(&self) -> Vec<String> {
        self.state
            .read()
            .expect("state lock")
            .append_system_prompt
            .clone()
    }

    fn get_append_system_prompt_sources(&self) -> Vec<PathBuf> {
        self.state
            .read()
            .expect("state lock")
            .append_system_prompt_sources
            .clone()
    }

    fn extend_resources(&self, paths: ResourceExtensionPaths) -> Result<(), String> {
        let _transaction = self
            .reload_lock
            .try_lock()
            .map_err(|_| "Resource loading is already in progress".to_owned())?;
        let mut candidate = self.state.read().expect("state lock").clone();
        let mut external = self
            .external_resources
            .read()
            .expect("external resources lock")
            .clone();
        for (path, info) in paths.skill_paths {
            let base = info
                .base_dir
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(|| self.cwd.clone());
            let path = if path.is_absolute() {
                path
            } else {
                base.join(path)
            };
            if !external
                .skill_paths
                .iter()
                .any(|(existing, _)| existing == &path)
            {
                external.skill_paths.push((path.clone(), info.clone()));
                candidate.dynamic_skill_paths.push((path, info));
            }
        }
        for (path, info) in paths.prompt_paths {
            let base = info
                .base_dir
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(|| self.cwd.clone());
            let path = if path.is_absolute() {
                path
            } else {
                base.join(path)
            };
            if !external
                .prompt_paths
                .iter()
                .any(|(existing, _)| existing == &path)
            {
                external.prompt_paths.push((path.clone(), info.clone()));
                candidate.dynamic_prompt_paths.push((path, info));
            }
        }
        // Overrides may inspect the loader; execute them without holding its state lock.
        let (skills, prompts, _, _, _, _, _) = self.reload_non_extension_resources(
            &candidate.dynamic_skill_paths,
            &candidate.dynamic_prompt_paths,
        );
        candidate.skills_result = skills;
        candidate.prompts_result = prompts;
        *self
            .external_resources
            .write()
            .expect("external resources lock") = external;
        *self.state.write().expect("state lock") = candidate;
        Ok(())
    }

    fn reload(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            let _guard = self.reload_lock.lock().await;
            let reason = if self.state.read().expect("state lock").loaded {
                "reload"
            } else {
                "startup"
            };
            let candidate = self.prepare_resources(reason).await?;
            *self.state.write().expect("state lock") = candidate;
            Ok(())
        })
    }

    fn load_session_resources(
        &self,
        reload: bool,
    ) -> BoxFuture<'_, Result<SessionResources, String>> {
        Box::pin(async move {
            let _guard = self.reload_lock.lock().await;
            if !reload {
                let mut st = self.state.write().expect("state lock");
                if st.unclaimed_runner.take().is_some() {
                    return Ok(Self::session_snapshot(&st));
                }
            }
            let mut candidate = self
                .prepare_resources(if reload { "reload" } else { "startup" })
                .await?;
            candidate.unclaimed_runner = None;
            let snapshot = Self::session_snapshot(&candidate);
            *self.state.write().expect("state lock") = candidate;
            Ok(snapshot)
        })
    }
}

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use pi_session::{
    PromptTemplateEntry, SourceInfo, expand_prompt_template_text, parse_template_command_args,
    substitute_template_args,
};
use serde::{Deserialize, Serialize};

use crate::settings_manager::CONFIG_DIR_NAME;
use crate::skills::{
    ResourceCollision, ResourceDiagnostic, parse_markdown_frontmatter, resource_path_key,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptTemplate {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
    pub content: String,
    pub source_info: SourceInfo,
    pub file_path: String,
}

impl PromptTemplate {
    pub fn to_prompt_entry(&self) -> PromptTemplateEntry {
        PromptTemplateEntry {
            name: self.name.clone(),
            description: self.description.clone(),
            argument_hint: self.argument_hint.clone(),
            content: self.content.clone(),
            source_info: self.source_info.clone(),
            file_path: self.file_path.clone(),
        }
    }
}

pub fn parse_command_args(args_string: &str) -> Vec<String> {
    parse_template_command_args(args_string)
}

pub fn substitute_args(content: &str, args: &[String]) -> String {
    substitute_template_args(content, args)
}

pub fn expand_prompt_template(text: &str, templates: &[PromptTemplate]) -> String {
    let entries: Vec<PromptTemplateEntry> = templates
        .iter()
        .map(PromptTemplate::to_prompt_entry)
        .collect();
    expand_prompt_template_text(text, &entries)
}

#[derive(Debug, Clone, Default)]
pub struct LoadPromptTemplatesResult {
    pub templates: Vec<PromptTemplate>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

#[derive(Debug, Clone)]
pub struct LoadPromptTemplatesOptions {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub prompt_paths: Vec<PathBuf>,
    pub include_defaults: bool,
}

fn create_template_source_info(file_path: &Path, base_dir: &Path, scope: &str) -> SourceInfo {
    let mut info = SourceInfo::synthetic(file_path.to_string_lossy().into_owned(), "local");
    info.base_dir = Some(base_dir.to_string_lossy().into_owned());
    info.scope = scope.to_owned();
    info
}

fn load_template_from_file(
    file_path: &Path,
    source_info: SourceInfo,
) -> (Option<PromptTemplate>, Vec<ResourceDiagnostic>) {
    let mut diagnostics = Vec::new();
    let path_str = file_path.to_string_lossy().into_owned();

    let raw_content = match fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(err) => {
            diagnostics.push(ResourceDiagnostic {
                kind: "warning".to_owned(),
                message: err.to_string(),
                path: Some(path_str),
                collision: None,
            });
            return (None, diagnostics);
        }
    };

    let parsed = match parse_markdown_frontmatter(&raw_content) {
        Ok(p) => p,
        Err(err) => {
            diagnostics.push(ResourceDiagnostic {
                kind: "warning".to_owned(),
                message: err,
                path: Some(path_str),
                collision: None,
            });
            return (None, diagnostics);
        }
    };

    let file_stem = file_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut description = parsed
        .fields
        .get("description")
        .cloned()
        .unwrap_or_default();
    if description.is_empty()
        && let Some(first_line) = parsed.body.lines().find(|l| !l.trim().is_empty())
    {
        let chars: Vec<char> = first_line.chars().collect();
        if chars.len() > 60 {
            description = format!("{}...", chars[..60].iter().collect::<String>());
        } else {
            description = first_line.to_owned();
        }
    }

    let argument_hint = parsed
        .fields
        .get("argument-hint")
        .filter(|s| !s.is_empty())
        .cloned();

    (
        Some(PromptTemplate {
            name: file_stem,
            description,
            argument_hint,
            content: parsed.body,
            source_info,
            file_path: path_str,
        }),
        diagnostics,
    )
}

fn load_templates_from_dir(dir: &Path, scope: &str) -> LoadPromptTemplatesResult {
    let mut templates = Vec::new();
    let mut diagnostics = Vec::new();

    if !dir.exists() {
        return LoadPromptTemplatesResult {
            templates,
            diagnostics,
        };
    }

    let Ok(read_dir) = fs::read_dir(dir) else {
        return LoadPromptTemplatesResult {
            templates,
            diagnostics,
        };
    };

    let mut entries: Vec<fs::DirEntry> = read_dir.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let full_path = entry.path();
        let Ok(meta) = fs::metadata(&full_path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".md") {
            continue;
        }
        let info = create_template_source_info(&full_path, dir, scope);
        let (tpl_opt, diags) = load_template_from_file(&full_path, info);
        if let Some(tpl) = tpl_opt {
            templates.push(tpl);
        }
        diagnostics.extend(diags);
    }

    LoadPromptTemplatesResult {
        templates,
        diagnostics,
    }
}

pub fn load_prompt_templates(options: &LoadPromptTemplatesOptions) -> LoadPromptTemplatesResult {
    let mut raw_result = LoadPromptTemplatesResult::default();
    let global_dir = options.agent_dir.join("prompts");
    let project_dir = options.cwd.join(CONFIG_DIR_NAME).join("prompts");

    if options.include_defaults {
        let g = load_templates_from_dir(&global_dir, "user");
        raw_result.templates.extend(g.templates);
        raw_result.diagnostics.extend(g.diagnostics);

        let p = load_templates_from_dir(&project_dir, "project");
        raw_result.templates.extend(p.templates);
        raw_result.diagnostics.extend(p.diagnostics);
    }

    for raw_path in &options.prompt_paths {
        let resolved = if raw_path.is_absolute() {
            raw_path.clone()
        } else {
            options.cwd.join(raw_path)
        };
        if !resolved.exists() {
            raw_result.diagnostics.push(ResourceDiagnostic {
                kind: "warning".to_owned(),
                message: "prompt path does not exist".to_owned(),
                path: Some(resolved.to_string_lossy().into_owned()),
                collision: None,
            });
            continue;
        }
        let Ok(meta) = fs::metadata(&resolved) else {
            continue;
        };
        let scope = if !options.include_defaults {
            if resolved.starts_with(&global_dir) {
                "user"
            } else if resolved.starts_with(&project_dir) {
                "project"
            } else {
                "temporary"
            }
        } else {
            "temporary"
        };

        if meta.is_dir() {
            let sub = load_templates_from_dir(&resolved, scope);
            raw_result.templates.extend(sub.templates);
            raw_result.diagnostics.extend(sub.diagnostics);
        } else if meta.is_file()
            && resolved
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        {
            let base = resolved.parent().unwrap_or_else(|| Path::new("."));
            let info = create_template_source_info(&resolved, base, scope);
            let (tpl_opt, diags) = load_template_from_file(&resolved, info);
            if let Some(tpl) = tpl_opt {
                raw_result.templates.push(tpl);
            }
            raw_result.diagnostics.extend(diags);
        } else {
            raw_result.diagnostics.push(ResourceDiagnostic {
                kind: "warning".to_owned(),
                message: "prompt path is not a markdown file".to_owned(),
                path: Some(resolved.to_string_lossy().into_owned()),
                collision: None,
            });
        }
    }

    let mut template_map: BTreeMap<String, usize> = BTreeMap::new();
    let mut deduped: Vec<PromptTemplate> = Vec::new();
    let mut real_paths: BTreeSet<String> = BTreeSet::new();
    let mut collisions: Vec<ResourceDiagnostic> = Vec::new();

    for tpl in raw_result.templates {
        let key = resource_path_key(Path::new(&tpl.file_path));
        if real_paths.contains(&key) {
            continue;
        }
        if let Some(&winner_idx) = template_map.get(&tpl.name) {
            let winner = &deduped[winner_idx];
            collisions.push(ResourceDiagnostic {
                kind: "collision".to_owned(),
                message: format!("name \"{}\" collision", tpl.name),
                path: Some(tpl.file_path.clone()),
                collision: Some(ResourceCollision {
                    resource_type: "prompt".to_owned(),
                    name: tpl.name.clone(),
                    winner_path: winner.file_path.clone(),
                    loser_path: tpl.file_path.clone(),
                }),
            });
        } else {
            real_paths.insert(key);
            let idx = deduped.len();
            template_map.insert(tpl.name.clone(), idx);
            deduped.push(tpl);
        }
    }

    raw_result.diagnostics.extend(collisions);
    LoadPromptTemplatesResult {
        templates: deduped,
        diagnostics: raw_result.diagnostics,
    }
}

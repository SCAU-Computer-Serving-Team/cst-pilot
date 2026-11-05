use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use pi_session::{SkillPromptEntry, SourceInfo};
use serde::{Deserialize, Serialize};

use crate::auth_storage::strip_utf8_bom;
use crate::settings_manager::CONFIG_DIR_NAME;

pub const MAX_SKILL_NAME_LENGTH: usize = 64;
pub const MAX_SKILL_DESCRIPTION_LENGTH: usize = 1024;
const IGNORE_FILE_NAMES: &[&str] = &[".gitignore", ".ignore", ".fdignore"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceCollision {
    pub resource_type: String,
    pub name: String,
    pub winner_path: String,
    pub loser_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceDiagnostic {
    #[serde(rename = "type")]
    pub kind: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collision: Option<ResourceCollision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub file_path: String,
    pub base_dir: String,
    pub source_info: SourceInfo,
    pub disable_model_invocation: bool,
}

impl Skill {
    pub fn to_prompt_entry(&self) -> SkillPromptEntry {
        SkillPromptEntry {
            name: self.name.clone(),
            description: self.description.clone(),
            file_path: self.file_path.clone(),
            base_dir: self.base_dir.clone(),
            source_info: self.source_info.clone(),
            disable_model_invocation: self.disable_model_invocation,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LoadSkillsResult {
    pub skills: Vec<Skill>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SkillReadToolKind {
    #[default]
    Read,
    Bash,
    Indirect,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedFrontmatter {
    pub fields: BTreeMap<String, String>,
    pub bool_fields: BTreeMap<String, bool>,
    pub body: String,
}

/// 解析 Markdown 文件顶部的 YAML frontmatter（支持单行字符串、引号包裹字符串、多行缩进折叠块与布尔值）。
pub fn parse_markdown_frontmatter(raw: &str) -> Result<ParsedFrontmatter, String> {
    let text = strip_utf8_bom(raw);
    let Some(after_open) = text
        .strip_prefix("---\r\n")
        .or_else(|| text.strip_prefix("---\n"))
    else {
        return Ok(ParsedFrontmatter {
            fields: BTreeMap::new(),
            bool_fields: BTreeMap::new(),
            body: text.to_owned(),
        });
    };

    let mut close_offset: Option<(usize, usize)> = None;
    let mut pos = 0usize;
    for line in after_open.split_inclusive('\n') {
        let trimmed_cr = line.trim_end_matches(['\r', '\n']);
        if trimmed_cr == "---" {
            close_offset = Some((pos, pos + line.len()));
            break;
        }
        pos += line.len();
    }

    let Some((yaml_end, body_start)) = close_offset else {
        return Err("unclosed frontmatter block (missing closing ---)".to_owned());
    };

    let yaml_section = &after_open[..yaml_end];
    let body = after_open[body_start..].to_owned();

    let mut fields = BTreeMap::new();
    let mut bool_fields = BTreeMap::new();
    let mut current_key: Option<String> = None;
    let mut current_multiline = String::new();

    for raw_line in yaml_section.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if (raw_line.starts_with(' ') || raw_line.starts_with('\t'))
            && let Some(ref k) = current_key
        {
            if !current_multiline.is_empty() {
                current_multiline.push(' ');
            }
            current_multiline.push_str(trimmed);
            fields.insert(k.clone(), current_multiline.clone());
            continue;
        }

        current_key = None;
        current_multiline.clear();

        let Some(colon_idx) = raw_line.find(':') else {
            return Err(format!("invalid frontmatter line: {trimmed}"));
        };
        let key = raw_line[..colon_idx].trim().to_owned();
        if key.is_empty() {
            return Err("empty frontmatter key".to_owned());
        }
        let val_part = raw_line[colon_idx + 1..].trim();
        if val_part == "|"
            || val_part == ">"
            || val_part == "|-"
            || val_part == ">-"
            || val_part.is_empty()
        {
            current_key = Some(key);
            continue;
        }
        let unquoted =
            if (val_part.starts_with('"') && val_part.ends_with('"') && val_part.len() >= 2)
                || (val_part.starts_with('\'') && val_part.ends_with('\'') && val_part.len() >= 2)
            {
                val_part[1..val_part.len() - 1].to_owned()
            } else {
                val_part.to_owned()
            };
        if unquoted == "true" {
            bool_fields.insert(key.clone(), true);
        } else if unquoted == "false" {
            bool_fields.insert(key.clone(), false);
        }
        fields.insert(key, unquoted);
    }

    Ok(ParsedFrontmatter {
        fields,
        bool_fields,
        body,
    })
}

#[derive(Clone, Default)]
pub(crate) struct GitIgnoreMatcher {
    rules: Vec<ignore::gitignore::Gitignore>,
    root_dir: Option<PathBuf>,
}

impl GitIgnoreMatcher {
    pub fn add_dir_rules(&mut self, dir: &Path, root_dir: &Path) {
        self.root_dir.get_or_insert_with(|| root_dir.to_path_buf());
        for filename in IGNORE_FILE_NAMES {
            let path = dir.join(filename);
            if !path.is_file() {
                continue;
            }
            let mut builder = ignore::gitignore::GitignoreBuilder::new(dir);
            builder.add(path);
            if let Ok(rules) = builder.build() {
                self.rules.push(rules);
            }
        }
    }

    pub fn is_ignored(&self, rel_posix_path: &str, is_dir: bool) -> bool {
        let Some(root) = &self.root_dir else {
            return false;
        };
        let path = root.join(rel_posix_path);
        let mut ignored = false;
        for rules in &self.rules {
            let matched = rules.matched_path_or_any_parents(&path, is_dir);
            if !matched.is_none() {
                ignored = matched.is_ignore();
            }
        }
        ignored
    }
}

pub(crate) fn to_posix_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

pub(crate) fn glob_fragment_to_regex(pat: &str) -> String {
    let chars: Vec<char> = pat.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '*' {
            if i + 1 < chars.len() && chars[i + 1] == '*' {
                if i + 2 < chars.len() && chars[i + 2] == '/' {
                    out.push_str("(?:.*/)?");
                    i += 3;
                    continue;
                }
                out.push_str(".*");
                i += 2;
                continue;
            }
            out.push_str("[^/]*");
            i += 1;
            continue;
        }
        if chars[i] == '?' {
            out.push_str("[^/]");
            i += 1;
            continue;
        }
        let ch = chars[i];
        if ".+()|^${}[]\\".contains(ch) {
            out.push('\\');
        }
        out.push(ch);
        i += 1;
    }
    out
}

fn validate_skill_name(name: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if name.len() > MAX_SKILL_NAME_LENGTH {
        errors.push(format!(
            "name exceeds {MAX_SKILL_NAME_LENGTH} characters ({})",
            name.len()
        ));
    }
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        errors.push(
            "name contains invalid characters (must be lowercase a-z, 0-9, hyphens only)"
                .to_owned(),
        );
    }
    if name.starts_with('-') || name.ends_with('-') {
        errors.push("name must not start or end with a hyphen".to_owned());
    }
    if name.contains("--") {
        errors.push("name must not contain consecutive hyphens".to_owned());
    }
    errors
}

fn validate_skill_description(description: Option<&str>) -> Vec<String> {
    let mut errors = Vec::new();
    match description {
        None => errors.push("description is required".to_owned()),
        Some(d) if d.trim().is_empty() => errors.push("description is required".to_owned()),
        Some(d) if d.chars().count() > MAX_SKILL_DESCRIPTION_LENGTH => errors.push(format!(
            "description exceeds {MAX_SKILL_DESCRIPTION_LENGTH} characters ({})",
            d.chars().count()
        )),
        Some(_) => {}
    }
    errors
}

fn create_skill_source_info(file_path: &Path, base_dir: &Path, source: &str) -> SourceInfo {
    let mut info = SourceInfo::synthetic(file_path.to_string_lossy().into_owned(), "local");
    info.base_dir = Some(base_dir.to_string_lossy().into_owned());
    match source {
        "user" | "project" => {
            info.scope = source.to_owned();
        }
        other => {
            info.source = other.to_owned();
        }
    }
    info
}

fn load_skill_from_file(
    file_path: &Path,
    source: &str,
) -> (Option<Skill>, Vec<ResourceDiagnostic>) {
    let mut diagnostics = Vec::new();
    let path_str = file_path.to_string_lossy().into_owned();
    let is_declared_skill = file_path
        .file_name()
        .is_some_and(|n| n.to_string_lossy() == "SKILL.md");

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
            if is_declared_skill {
                diagnostics.push(ResourceDiagnostic {
                    kind: "warning".to_owned(),
                    message: err,
                    path: Some(path_str),
                    collision: None,
                });
            }
            return (None, diagnostics);
        }
    };

    let description = parsed.fields.get("description").map(String::as_str);
    let has_description = description.is_some_and(|d| !d.trim().is_empty());
    if !is_declared_skill && !has_description {
        return (None, diagnostics);
    }

    let skill_dir = file_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let parent_dir_name = skill_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    for err in validate_skill_description(description) {
        diagnostics.push(ResourceDiagnostic {
            kind: "warning".to_owned(),
            message: err,
            path: Some(path_str.clone()),
            collision: None,
        });
    }

    let name = parsed
        .fields
        .get("name")
        .filter(|s| !s.is_empty())
        .cloned()
        .unwrap_or(parent_dir_name);

    for err in validate_skill_name(&name) {
        diagnostics.push(ResourceDiagnostic {
            kind: "warning".to_owned(),
            message: err,
            path: Some(path_str.clone()),
            collision: None,
        });
    }

    if !has_description {
        return (None, diagnostics);
    }

    let disable_model_invocation = parsed
        .bool_fields
        .get("disable-model-invocation")
        .copied()
        .unwrap_or(false);

    let skill = Skill {
        name,
        description: description.unwrap_or_default().to_owned(),
        file_path: path_str,
        base_dir: skill_dir.to_string_lossy().into_owned(),
        source_info: create_skill_source_info(file_path, &skill_dir, source),
        disable_model_invocation,
    };
    (Some(skill), diagnostics)
}

pub fn load_skills_from_dir(dir: &Path, source: &str) -> LoadSkillsResult {
    let mut visited_dirs = BTreeSet::new();
    let mut ignore = GitIgnoreMatcher::default();
    load_skills_from_dir_internal(dir, source, true, &mut ignore, dir, &mut visited_dirs)
}

fn load_skills_from_dir_internal(
    dir: &Path,
    source: &str,
    include_root_files: bool,
    ignore: &mut GitIgnoreMatcher,
    root_dir: &Path,
    visited_dirs: &mut BTreeSet<PathBuf>,
) -> LoadSkillsResult {
    let mut skills = Vec::new();
    let mut diagnostics = Vec::new();

    if !dir.exists() {
        return LoadSkillsResult {
            skills,
            diagnostics,
        };
    }

    let canonical_dir = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    if !visited_dirs.insert(canonical_dir) {
        return LoadSkillsResult {
            skills,
            diagnostics,
        };
    }

    ignore.add_dir_rules(dir, root_dir);

    let Ok(read_dir) = fs::read_dir(dir) else {
        return LoadSkillsResult {
            skills,
            diagnostics,
        };
    };

    let mut entries: Vec<fs::DirEntry> = read_dir.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());

    // 1. Check if SKILL.md exists directly in this directory; if so, treat as skill root and stop recursing.
    for entry in &entries {
        if entry.file_name().to_string_lossy() != "SKILL.md" {
            continue;
        }
        let full_path = entry.path();
        let Ok(meta) = fs::metadata(&full_path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let rel_path = full_path
            .strip_prefix(root_dir)
            .ok()
            .map(to_posix_path)
            .unwrap_or_default();
        if ignore.is_ignored(&rel_path, false) {
            continue;
        }
        let (skill_opt, diags) = load_skill_from_file(&full_path, source);
        if let Some(s) = skill_opt {
            skills.push(s);
        }
        diagnostics.extend(diags);
        return LoadSkillsResult {
            skills,
            diagnostics,
        };
    }

    // 2. Otherwise scan subdirectories and (when include_root_files) direct .md files
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let full_path = entry.path();
        let Ok(meta) = fs::metadata(&full_path) else {
            continue;
        };
        let is_dir = meta.is_dir();
        let is_file = meta.is_file();

        let rel_path = full_path
            .strip_prefix(root_dir)
            .ok()
            .map(to_posix_path)
            .unwrap_or_default();
        if ignore.is_ignored(&rel_path, is_dir) {
            continue;
        }

        if is_dir {
            let mut child_ignore = ignore.clone();
            let sub = load_skills_from_dir_internal(
                &full_path,
                source,
                false,
                &mut child_ignore,
                root_dir,
                visited_dirs,
            );
            skills.extend(sub.skills);
            diagnostics.extend(sub.diagnostics);
        } else if is_file && include_root_files && name.ends_with(".md") {
            let (skill_opt, diags) = load_skill_from_file(&full_path, source);
            if let Some(s) = skill_opt {
                skills.push(s);
            }
            diagnostics.extend(diags);
        }
    }

    LoadSkillsResult {
        skills,
        diagnostics,
    }
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// 将可见技能格式化为系统提示词中的 `<available_skills>` XML 块（对照 `formatSkillsForPrompt`）。
pub fn format_skills_for_prompt(skills: &[Skill], read_tool: SkillReadToolKind) -> String {
    let visible: Vec<&Skill> = skills
        .iter()
        .filter(|s| !s.disable_model_invocation)
        .collect();
    if visible.is_empty() {
        return String::new();
    }

    let tool_line = match read_tool {
        SkillReadToolKind::Read => {
            "Use the read tool to load a skill's file when the task matches its description."
        }
        SkillReadToolKind::Bash => {
            "Use bash to load a skill's file when the task matches its description."
        }
        SkillReadToolKind::Indirect => "Load a skill's file when the task matches its description.",
    };

    let mut lines = vec![
        "\n\nThe following skills provide specialized instructions for specific tasks.".to_owned(),
        tool_line.to_owned(),
        "Resolve relative paths against the skill's directory (parent of SKILL.md). Use absolute paths in tool calls.".to_owned(),
        String::new(),
        "<available_skills>".to_owned(),
    ];

    for skill in visible {
        lines.push("  <skill>".to_owned());
        lines.push(format!("    <name>{}</name>", escape_xml(&skill.name)));
        lines.push(format!(
            "    <description>{}</description>",
            escape_xml(&skill.description)
        ));
        lines.push(format!(
            "    <location>{}</location>",
            escape_xml(&skill.file_path)
        ));
        lines.push("  </skill>".to_owned());
    }

    lines.push("</available_skills>".to_owned());
    lines.join("\n")
}

pub(crate) fn resource_path_key(path: &Path) -> String {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let s = canonical.to_string_lossy().into_owned();
    #[cfg(windows)]
    {
        s.strip_prefix(r"\\?\")
            .unwrap_or(&s)
            .replace('/', "\\")
            .to_lowercase()
    }
    #[cfg(not(windows))]
    {
        s
    }
}

#[derive(Debug, Clone)]
pub struct LoadSkillsOptions {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub skill_paths: Vec<PathBuf>,
    pub include_defaults: bool,
}

pub fn load_skills(options: &LoadSkillsOptions) -> LoadSkillsResult {
    let mut skill_map: BTreeMap<String, usize> = BTreeMap::new();
    let mut ordered_skills: Vec<Skill> = Vec::new();
    let mut real_path_set: BTreeSet<String> = BTreeSet::new();
    let mut all_diagnostics: Vec<ResourceDiagnostic> = Vec::new();
    let mut collision_diagnostics: Vec<ResourceDiagnostic> = Vec::new();

    let mut add_skills = |res: LoadSkillsResult, all_diagnostics: &mut Vec<ResourceDiagnostic>| {
        all_diagnostics.extend(res.diagnostics);
        for skill in res.skills {
            let real_key = resource_path_key(Path::new(&skill.file_path));
            if real_path_set.contains(&real_key) {
                continue;
            }
            if let Some(&existing_idx) = skill_map.get(&skill.name) {
                let winner = &ordered_skills[existing_idx];
                collision_diagnostics.push(ResourceDiagnostic {
                    kind: "collision".to_owned(),
                    message: format!("name \"{}\" collision", skill.name),
                    path: Some(skill.file_path.clone()),
                    collision: Some(ResourceCollision {
                        resource_type: "skill".to_owned(),
                        name: skill.name.clone(),
                        winner_path: winner.file_path.clone(),
                        loser_path: skill.file_path.clone(),
                    }),
                });
            } else {
                real_path_set.insert(real_key);
                let idx = ordered_skills.len();
                skill_map.insert(skill.name.clone(), idx);
                ordered_skills.push(skill);
            }
        }
    };

    let user_skills_dir = options.agent_dir.join("skills");
    let project_skills_dir = options.cwd.join(CONFIG_DIR_NAME).join("skills");

    if options.include_defaults {
        add_skills(
            load_skills_from_dir(&user_skills_dir, "user"),
            &mut all_diagnostics,
        );
        add_skills(
            load_skills_from_dir(&project_skills_dir, "project"),
            &mut all_diagnostics,
        );
    }

    for raw_path in &options.skill_paths {
        let resolved = if raw_path.is_absolute() {
            raw_path.clone()
        } else {
            options.cwd.join(raw_path)
        };
        if !resolved.exists() {
            all_diagnostics.push(ResourceDiagnostic {
                kind: "warning".to_owned(),
                message: "skill path does not exist".to_owned(),
                path: Some(resolved.to_string_lossy().into_owned()),
                collision: None,
            });
            continue;
        }
        let Ok(meta) = fs::metadata(&resolved) else {
            continue;
        };
        let source = if !options.include_defaults {
            if resolved.starts_with(&user_skills_dir) {
                "user"
            } else if resolved.starts_with(&project_skills_dir) {
                "project"
            } else {
                "path"
            }
        } else {
            "path"
        };

        if meta.is_dir() {
            add_skills(
                load_skills_from_dir(&resolved, source),
                &mut all_diagnostics,
            );
        } else if meta.is_file()
            && resolved
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        {
            let (skill_opt, diags) = load_skill_from_file(&resolved, source);
            if let Some(s) = skill_opt {
                add_skills(
                    LoadSkillsResult {
                        skills: vec![s],
                        diagnostics: diags,
                    },
                    &mut all_diagnostics,
                );
            } else {
                all_diagnostics.extend(diags);
            }
        } else {
            all_diagnostics.push(ResourceDiagnostic {
                kind: "warning".to_owned(),
                message: "skill path is not a markdown file".to_owned(),
                path: Some(resolved.to_string_lossy().into_owned()),
                collision: None,
            });
        }
    }

    all_diagnostics.extend(collision_diagnostics);
    LoadSkillsResult {
        skills: ordered_skills,
        diagnostics: all_diagnostics,
    }
}

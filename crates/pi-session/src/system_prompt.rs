use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use pi_ai::{SystemContent, SystemMessage, get_system_message_text};
use regex::Regex;
use serde::{Deserialize, Serialize};

pub type SystemPromptSections = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptContextFile {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptSkill {
    pub name: String,
    pub description: String,
    pub file_path: String,
    pub base_dir: String,
    #[serde(default)]
    pub disable_model_invocation: bool,
}

/// 系统提示词构造参数（对照 `BuildSystemPromptOptions`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildSystemPromptOptions {
    pub custom_prompt: Option<String>,
    pub force_system_prompt: Option<String>,
    pub selected_tools: Option<Vec<String>>,
    pub hidden_tools: Option<Vec<String>>,
    pub tool_snippets: Option<BTreeMap<String, String>>,
    pub tool_guidelines: Option<BTreeMap<String, Vec<String>>>,
    pub prompt_guidelines: Option<Vec<String>>,
    pub append_system_prompt: Option<String>,
    pub sections: Option<BTreeMap<String, String>>,
    pub cwd: String,
    pub context_files: Option<Vec<PromptContextFile>>,
    pub skills: Option<Vec<PromptSkill>>,
}

/// 归一化后的系统提示词配置（`before_agent_start` 通过共享更新接口读写）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedBuildSystemPromptOptions {
    pub custom_prompt: Option<String>,
    pub force_system_prompt: Option<String>,
    pub selected_tools: Vec<String>,
    pub hidden_tools: Vec<String>,
    pub tool_snippets: BTreeMap<String, String>,
    pub tool_guidelines: BTreeMap<String, Vec<String>>,
    pub prompt_guidelines: Vec<String>,
    pub append_system_prompt: String,
    pub sections: BTreeMap<String, String>,
    pub cwd: String,
    pub context_files: Vec<PromptContextFile>,
    pub skills: Vec<PromptSkill>,
}

impl NormalizedBuildSystemPromptOptions {
    pub fn to_build_options(&self) -> BuildSystemPromptOptions {
        BuildSystemPromptOptions {
            custom_prompt: self.custom_prompt.clone(),
            force_system_prompt: self.force_system_prompt.clone(),
            selected_tools: Some(self.selected_tools.clone()),
            hidden_tools: Some(self.hidden_tools.clone()),
            tool_snippets: Some(self.tool_snippets.clone()),
            tool_guidelines: Some(self.tool_guidelines.clone()),
            prompt_guidelines: Some(self.prompt_guidelines.clone()),
            append_system_prompt: Some(self.append_system_prompt.clone()),
            sections: Some(self.sections.clone()),
            cwd: self.cwd.clone(),
            context_files: Some(self.context_files.clone()),
            skills: Some(self.skills.clone()),
        }
    }
}

pub fn normalize_build_system_prompt_options(
    input: &BuildSystemPromptOptions,
) -> NormalizedBuildSystemPromptOptions {
    NormalizedBuildSystemPromptOptions {
        custom_prompt: input.custom_prompt.clone(),
        force_system_prompt: input.force_system_prompt.clone(),
        selected_tools: input.selected_tools.clone().unwrap_or_else(|| {
            vec![
                "read".to_owned(),
                "bash".to_owned(),
                "edit".to_owned(),
                "write".to_owned(),
            ]
        }),
        hidden_tools: input.hidden_tools.clone().unwrap_or_default(),
        tool_snippets: input.tool_snippets.clone().unwrap_or_default(),
        tool_guidelines: input.tool_guidelines.clone().unwrap_or_default(),
        prompt_guidelines: input.prompt_guidelines.clone().unwrap_or_default(),
        append_system_prompt: input.append_system_prompt.clone().unwrap_or_default(),
        sections: input.sections.clone().unwrap_or_default(),
        cwd: input.cwd.clone(),
        context_files: input.context_files.clone().unwrap_or_default(),
        skills: input.skills.clone().unwrap_or_default(),
    }
}

fn section_name_regex() -> &'static Regex {
    static LOCK: OnceLock<Regex> = OnceLock::new();
    LOCK.get_or_init(|| Regex::new(r"^[a-z][a-z0-9_-]*$").expect("valid section name regex"))
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn format_skills_for_prompt(skills: &[PromptSkill], file_read_tool: &str) -> String {
    let visible: Vec<&PromptSkill> = skills
        .iter()
        .filter(|s| !s.disable_model_invocation)
        .collect();
    if visible.is_empty() {
        return String::new();
    }

    let tool_hint = match file_read_tool {
        "read" => "Use the read tool to load a skill's file when the task matches its description.",
        "bash" => "Use bash to load a skill's file when the task matches its description.",
        _ => "Load a skill's file when the task matches its description.",
    };

    let mut lines = vec![
        "\n\nThe following skills provide specialized instructions for specific tasks.".to_owned(),
        tool_hint.to_owned(),
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

fn render_project_context(context_files: &[PromptContextFile]) -> String {
    let mut parts = Vec::with_capacity(context_files.len() + 1);
    parts.push("Project-specific instructions and guidelines:".to_owned());
    for file in context_files {
        parts.push(format!(
            "<project_instructions path=\"{}\">\n{}\n</project_instructions>",
            file.path, file.content
        ));
    }
    parts.join("\n\n")
}

fn build_rules(
    selected_tools: &[String],
    tool_guidelines: &BTreeMap<String, Vec<String>>,
    prompt_guidelines: &[String],
) -> String {
    let mut rules: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    let mut add_rule = |rule: &str| {
        let normalized = rule.trim();
        if normalized.is_empty() || seen.contains(normalized) {
            return;
        }
        seen.insert(normalized.to_owned());
        rules.push(normalized.to_owned());
    };

    let has_bash = selected_tools.iter().any(|t| t == "bash");
    let has_powershell = selected_tools.iter().any(|t| t == "powershell");
    let has_grep = selected_tools.iter().any(|t| t == "grep");
    let has_find = selected_tools.iter().any(|t| t == "find");
    let has_ls = selected_tools.iter().any(|t| t == "ls");

    if (has_bash || has_powershell) && !has_grep && !has_find && !has_ls {
        if has_bash && has_powershell {
            add_rule(
                "Use bash or PowerShell for file operations like listing, searching, and finding files",
            );
        } else if has_powershell {
            add_rule(
                "Use PowerShell for file operations like listing, searching, and finding files",
            );
        } else {
            add_rule("Use bash for file operations like ls, rg, find");
        }
    }

    for name in selected_tools {
        if let Some(guidelines) = tool_guidelines.get(name) {
            for rule in guidelines {
                add_rule(rule);
            }
        }
    }
    for rule in prompt_guidelines {
        add_rule(rule);
    }
    add_rule("Be concise in your responses");
    add_rule("Show file paths clearly when working with files");

    rules
        .into_iter()
        .map(|r| format!("- {r}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 构建命名系统提示词段落集合（对照 `buildSystemPromptSections`）。
pub fn build_system_prompt_sections(
    input: &BuildSystemPromptOptions,
) -> Result<SystemPromptSections, String> {
    let options = normalize_build_system_prompt_options(input);

    for name in options.sections.keys() {
        if !section_name_regex().is_match(name) || name == "preamble" {
            return Err(format!("Invalid system prompt section name: {name}"));
        }
    }

    let declared_tools: Vec<String> = options
        .selected_tools
        .iter()
        .filter(|name| !options.hidden_tools.contains(name))
        .cloned()
        .collect();

    let mut prompt_sections: BTreeMap<String, String> = BTreeMap::new();
    if let Some(custom_prompt) = &options.custom_prompt
        && !custom_prompt.is_empty()
    {
        prompt_sections.insert("preamble".to_owned(), custom_prompt.clone());
    } else {
        prompt_sections.insert(
            "preamble".to_owned(),
            "You are a coding assistant running inside pi, a coding agent harness. Help users inspect files, run commands, and modify code.".to_owned(),
        );
        let visible_tools: Vec<String> = declared_tools
            .iter()
            .filter_map(|name| {
                options
                    .tool_snippets
                    .get(name)
                    .filter(|s| !s.is_empty())
                    .map(|snippet| format!("- {name}: {snippet}"))
            })
            .collect();
        let tools_body = if visible_tools.is_empty() {
            "(none)".to_owned()
        } else {
            visible_tools.join("\n")
        };
        prompt_sections.insert(
            "tools".to_owned(),
            format!("{tools_body}\n\nAdditional tools may be available."),
        );
        prompt_sections.insert(
            "rules".to_owned(),
            build_rules(
                &declared_tools,
                &options.tool_guidelines,
                &options.prompt_guidelines,
            ),
        );
        prompt_sections.insert(
            "docs".to_owned(),
            "Pi documentation (read only when the user asks about the harness itself, its SDK, extensions, themes, skills, or TUI):\n- Before implementing harness changes, read the relevant docs and examples in full, following related cross-references.".to_owned(),
        );
    }

    if !options.append_system_prompt.is_empty() {
        prompt_sections.insert("addendum".to_owned(), options.append_system_prompt.clone());
    }
    if !options.context_files.is_empty() {
        prompt_sections.insert(
            "project_context".to_owned(),
            render_project_context(&options.context_files),
        );
    }

    let skill_file_read_tool = ["read", "bash"]
        .into_iter()
        .find(|t| declared_tools.iter().any(|d| d == t))
        .or_else(|| {
            ["read", "bash"]
                .into_iter()
                .any(|t| options.selected_tools.iter().any(|s| s == t))
                .then_some("indirect")
        });

    if let Some(read_tool) = skill_file_read_tool
        && !options.skills.is_empty()
    {
        let skills_prompt = format_skills_for_prompt(&options.skills, read_tool)
            .trim()
            .to_owned();
        if !skills_prompt.is_empty() {
            prompt_sections.insert("skills".to_owned(), skills_prompt);
        }
    }

    prompt_sections.insert("cwd".to_owned(), options.cwd.replace('\\', "/"));

    for (name, content) in &options.sections {
        if !content.is_empty() {
            prompt_sections.insert(name.clone(), content.clone());
        }
    }

    let mut sections: SystemPromptSections = BTreeMap::new();
    if let Some(preamble) = prompt_sections.remove("preamble") {
        sections.insert("preamble".to_owned(), preamble);
    }
    for (name, content) in prompt_sections {
        sections.insert(name.clone(), format!("<{name}>\n{content}\n</{name}>"));
    }
    Ok(sections)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPromptState {
    pub content: String,
    pub sections: Option<SystemPromptSections>,
}

impl SystemPromptState {
    pub fn to_system_message(&self, timestamp: u64) -> SystemMessage {
        let sections = self.sections.as_ref().map(|map| {
            map.iter()
                .map(|(k, v)| (k.clone(), Some(v.clone())))
                .collect::<BTreeMap<String, Option<String>>>()
        });
        SystemMessage {
            content: SystemContent::Text(self.content.clone()),
            sections,
            tools_added: None,
            tools_removed: None,
            timestamp,
        }
    }
}

/// 构建完整系统提示词状态（若设置了 `force_system_prompt` 则退化为不带 `sections` 的纯文本 `content`）。
pub fn build_system_prompt_state(
    input: &BuildSystemPromptOptions,
) -> Result<SystemPromptState, String> {
    if let Some(forced) = &input.force_system_prompt {
        return Ok(SystemPromptState {
            content: forced.clone(),
            sections: None,
        });
    }
    Ok(SystemPromptState {
        content: String::new(),
        sections: Some(build_system_prompt_sections(input)?),
    })
}

/// 构建渲染后的完整系统提示词文本（对照 `buildSystemPrompt`）。
pub fn build_system_prompt(input: &BuildSystemPromptOptions) -> Result<String, String> {
    let state = build_system_prompt_state(input)?;
    let sys_msg = state.to_system_message(0);
    Ok(get_system_message_text(&sys_msg))
}

/// 计算上一轮重放出的 `sections` 与当前目标 `sections` 之间的差量补丁（对照 `diffSystemPromptSections`）。
pub fn diff_system_prompt_sections(
    previous: &BTreeMap<String, Option<String>>,
    current: &SystemPromptSections,
) -> Option<BTreeMap<String, Option<String>>> {
    let mut patch: BTreeMap<String, Option<String>> = BTreeMap::new();
    for (name, text) in current {
        if previous.get(name).and_then(Option::as_ref) != Some(text) {
            patch.insert(name.clone(), Some(text.clone()));
        }
    }
    for (name, prev_val) in previous {
        if prev_val.is_some() && !current.contains_key(name) {
            patch.insert(name.clone(), None);
        }
    }
    if patch.is_empty() { None } else { Some(patch) }
}

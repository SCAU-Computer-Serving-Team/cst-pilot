pub mod bash;
pub mod edit;
pub mod edit_diff;
pub mod file_mutation_queue;
pub mod find;
pub mod grep;
pub mod ls;
pub mod powershell;
pub(crate) mod process_io;
pub mod read;
mod search_process;
pub use search_process::SearchBackend;
pub mod truncate;
pub mod write;

pub use bash::*;
pub use edit::*;
pub use edit_diff::*;
pub use file_mutation_queue::*;
pub use find::*;
pub use grep::*;
pub use ls::*;
pub use powershell::*;
pub use read::*;
pub use truncate::*;
pub use write::*;

use pi_agent::AgentTool;
use pi_session::ToolDefinition;
use std::collections::BTreeMap;

pub const ALL_BUILTIN_TOOL_NAMES: &[&str] = &[
    "read",
    "bash",
    "powershell",
    "edit",
    "write",
    "grep",
    "find",
    "ls",
];

#[derive(Clone, Default)]
pub struct ToolsOptions {
    pub read: Option<ReadToolOptions>,
    pub bash: Option<BashToolOptions>,
    pub powershell: Option<PowerShellToolOptions>,
    pub edit: Option<EditToolOptions>,
    pub write: Option<WriteToolOptions>,
    pub grep: Option<GrepToolOptions>,
    pub find: Option<FindToolOptions>,
    pub ls: Option<LsToolOptions>,
}

pub fn create_tool_definition(
    tool_name: &str,
    cwd: impl Into<String>,
    options: Option<&ToolsOptions>,
) -> Result<ToolDefinition, String> {
    let cwd = cwd.into();
    match tool_name {
        "read" => Ok(create_read_tool_definition(
            cwd,
            options.and_then(|o| o.read.clone()),
        )),
        "bash" => Ok(create_bash_tool_definition(
            cwd,
            options.and_then(|o| o.bash.clone()),
        )),
        "powershell" => Ok(create_powershell_tool_definition(
            cwd,
            options.and_then(|o| o.powershell.clone()),
        )),
        "edit" => Ok(create_edit_tool_definition(
            cwd,
            options.and_then(|o| o.edit.clone()),
        )),
        "write" => Ok(create_write_tool_definition(
            cwd,
            options.and_then(|o| o.write.clone()),
        )),
        "grep" => Ok(create_grep_tool_definition(
            cwd,
            options.and_then(|o| o.grep.clone()),
        )),
        "find" => Ok(create_find_tool_definition(
            cwd,
            options.and_then(|o| o.find.clone()),
        )),
        "ls" => Ok(create_ls_tool_definition(
            cwd,
            options.and_then(|o| o.ls.clone()),
        )),
        other => Err(format!("Unknown built-in tool name: {other}")),
    }
}

pub fn create_coding_tool_definitions(
    cwd: impl Into<String>,
    options: Option<&ToolsOptions>,
) -> Vec<ToolDefinition> {
    let cwd = cwd.into();
    vec![
        create_read_tool_definition(cwd.clone(), options.and_then(|o| o.read.clone())),
        create_bash_tool_definition(cwd.clone(), options.and_then(|o| o.bash.clone())),
        create_edit_tool_definition(cwd.clone(), options.and_then(|o| o.edit.clone())),
        create_write_tool_definition(cwd, options.and_then(|o| o.write.clone())),
    ]
}

pub fn create_read_only_tool_definitions(
    cwd: impl Into<String>,
    options: Option<&ToolsOptions>,
) -> Vec<ToolDefinition> {
    let cwd = cwd.into();
    vec![
        create_read_tool_definition(cwd.clone(), options.and_then(|o| o.read.clone())),
        create_grep_tool_definition(cwd.clone(), options.and_then(|o| o.grep.clone())),
        create_find_tool_definition(cwd.clone(), options.and_then(|o| o.find.clone())),
        create_ls_tool_definition(cwd, options.and_then(|o| o.ls.clone())),
    ]
}

pub fn create_all_tool_definitions(
    cwd: impl Into<String>,
    options: Option<&ToolsOptions>,
) -> BTreeMap<String, ToolDefinition> {
    let cwd = cwd.into();
    let mut map = BTreeMap::new();
    for &name in ALL_BUILTIN_TOOL_NAMES {
        if let Ok(def) = create_tool_definition(name, cwd.clone(), options) {
            map.insert(name.to_owned(), def);
        }
    }
    map
}

pub fn create_coding_tools(
    cwd: impl Into<String>,
    options: Option<&ToolsOptions>,
) -> Vec<AgentTool> {
    let cwd = cwd.into();
    vec![
        create_read_tool(cwd.clone(), options.and_then(|o| o.read.clone())),
        create_bash_tool(cwd.clone(), options.and_then(|o| o.bash.clone())),
        create_edit_tool(cwd.clone(), options.and_then(|o| o.edit.clone())),
        create_write_tool(cwd, options.and_then(|o| o.write.clone())),
    ]
}

pub fn create_read_only_tools(
    cwd: impl Into<String>,
    options: Option<&ToolsOptions>,
) -> Vec<AgentTool> {
    let cwd = cwd.into();
    vec![
        create_read_tool(cwd.clone(), options.and_then(|o| o.read.clone())),
        create_grep_tool(cwd.clone(), options.and_then(|o| o.grep.clone())),
        create_find_tool(cwd.clone(), options.and_then(|o| o.find.clone())),
        create_ls_tool(cwd, options.and_then(|o| o.ls.clone())),
    ]
}

pub fn create_all_tools(
    cwd: impl Into<String>,
    options: Option<&ToolsOptions>,
) -> BTreeMap<String, AgentTool> {
    let cwd = cwd.into();
    let mut map = BTreeMap::new();
    for &name in ALL_BUILTIN_TOOL_NAMES {
        if let Ok(def) = create_tool_definition(name, cwd.clone(), options) {
            let r = pi_session::ExtensionRunner::empty(cwd.clone());
            map.insert(
                name.to_owned(),
                def.to_agent_tool(move |tc_id, cancel| r.create_tool_context(tc_id, cancel)),
            );
        }
    }
    map
}

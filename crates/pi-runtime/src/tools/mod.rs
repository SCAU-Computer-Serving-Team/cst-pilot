#[cfg(feature = "coding-tools")]
pub mod bash;
#[cfg(feature = "coding-tools")]
pub mod edit;
#[cfg(feature = "coding-tools")]
pub mod edit_diff;
#[cfg(feature = "coding-tools")]
pub mod file_mutation_queue;
#[cfg(feature = "coding-tools")]
pub mod find;
#[cfg(feature = "coding-tools")]
pub mod grep;
#[cfg(feature = "coding-tools")]
pub mod ls;
#[cfg(feature = "coding-tools")]
pub mod powershell;
pub(crate) mod process_io;
pub mod read;
#[cfg(feature = "coding-tools")]
mod search_process;
#[cfg(feature = "coding-tools")]
pub use search_process::SearchBackend;
pub mod truncate;
#[cfg(feature = "coding-tools")]
pub mod write;

#[cfg(feature = "coding-tools")]
pub use bash::*;
#[cfg(feature = "coding-tools")]
pub use edit::*;
#[cfg(feature = "coding-tools")]
pub use edit_diff::*;
#[cfg(feature = "coding-tools")]
pub use file_mutation_queue::*;
#[cfg(feature = "coding-tools")]
pub use find::*;
#[cfg(feature = "coding-tools")]
pub use grep::*;
#[cfg(feature = "coding-tools")]
pub use ls::*;
#[cfg(feature = "coding-tools")]
pub use powershell::*;
pub use read::*;
pub use truncate::*;
#[cfg(feature = "coding-tools")]
pub use write::*;

use pi_agent::AgentTool;
use pi_session::ToolDefinition;
use std::collections::BTreeMap;

pub const ALL_BUILTIN_TOOL_NAMES: &[&str] = &[
    "read",
    #[cfg(feature = "coding-tools")]
    "bash",
    #[cfg(feature = "coding-tools")]
    "powershell",
    #[cfg(feature = "coding-tools")]
    "edit",
    #[cfg(feature = "coding-tools")]
    "write",
    #[cfg(feature = "coding-tools")]
    "grep",
    #[cfg(feature = "coding-tools")]
    "find",
    #[cfg(feature = "coding-tools")]
    "ls",
];

#[derive(Clone, Default)]
pub struct ToolsOptions {
    pub read: Option<ReadToolOptions>,
    #[cfg(feature = "coding-tools")]
    pub bash: Option<BashToolOptions>,
    #[cfg(feature = "coding-tools")]
    pub powershell: Option<PowerShellToolOptions>,
    #[cfg(feature = "coding-tools")]
    pub edit: Option<EditToolOptions>,
    #[cfg(feature = "coding-tools")]
    pub write: Option<WriteToolOptions>,
    #[cfg(feature = "coding-tools")]
    pub grep: Option<GrepToolOptions>,
    #[cfg(feature = "coding-tools")]
    pub find: Option<FindToolOptions>,
    #[cfg(feature = "coding-tools")]
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
        #[cfg(feature = "coding-tools")]
        "bash" => Ok(create_bash_tool_definition(
            cwd,
            options.and_then(|o| o.bash.clone()),
        )),
        #[cfg(feature = "coding-tools")]
        "powershell" => Ok(create_powershell_tool_definition(
            cwd,
            options.and_then(|o| o.powershell.clone()),
        )),
        #[cfg(feature = "coding-tools")]
        "edit" => Ok(create_edit_tool_definition(
            cwd,
            options.and_then(|o| o.edit.clone()),
        )),
        #[cfg(feature = "coding-tools")]
        "write" => Ok(create_write_tool_definition(
            cwd,
            options.and_then(|o| o.write.clone()),
        )),
        #[cfg(feature = "coding-tools")]
        "grep" => Ok(create_grep_tool_definition(
            cwd,
            options.and_then(|o| o.grep.clone()),
        )),
        #[cfg(feature = "coding-tools")]
        "find" => Ok(create_find_tool_definition(
            cwd,
            options.and_then(|o| o.find.clone()),
        )),
        #[cfg(feature = "coding-tools")]
        "ls" => Ok(create_ls_tool_definition(
            cwd,
            options.and_then(|o| o.ls.clone()),
        )),
        other => Err(format!("Unknown built-in tool name: {other}")),
    }
}

#[cfg(feature = "coding-tools")]
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

#[cfg(feature = "coding-tools")]
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

#[cfg(feature = "coding-tools")]
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

#[cfg(feature = "coding-tools")]
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

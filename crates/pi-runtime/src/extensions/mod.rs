pub mod codemode;
mod codemode_models;
pub mod mcp;
mod mcp_runtime;
pub mod tool_search;

use crate::resource_loader::BuiltinExtensionDescriptor;
use crate::settings_manager::SettingsManager;

pub use codemode::{
    CODEMODE_DESCRIPTION, CODEMODE_ONLY_GUIDELINE, CODEMODE_STORE_ENTRY_TYPE, CODEMODE_TOOL_NAME,
    CodemodeDescriptionOptions, CodemodeExtensionOptions, CodemodeMode, DEFAULT_INLINE_BUDGET,
    DEFAULT_MAX_CODEMODE_WORKERS, DEFAULT_MAX_OUTPUT_TOKENS, create_codemode_description,
    create_codemode_extension, create_codemode_tool_definition,
};
pub use mcp::{
    ConnectedMcpResourceServer, MCP_OUTPUT_MAX_BYTES, McpExtensionOptions, McpToolDetails,
    PreconnectedMcpServer, convert_mcp_result, create_default_transport, create_mcp_extension,
    create_mcp_resource_tool_definitions, create_mcp_result_schema, create_mcp_tool_definition,
    create_mcp_tool_name, limit_mcp_content, to_model_content, to_tool_exposure,
};
pub use tool_search::{
    Bm25Ranker, DEFAULT_TOOL_SEARCH_LIMIT, TOOL_SEARCH_DESCRIPTION, TOOL_SEARCH_TOOL_NAME,
    ToolRanker, ToolSearchDocument, ToolSearchInventory, ToolSearchMatch, ToolSearchResultTool,
    ToolSearchToolDetails, ToolSearchToolOptions, create_tool_search_document,
    create_tool_search_document_from_info, create_tool_search_extension,
    create_tool_search_tool_definition, is_searchable_exposure, tokenize,
};

pub(crate) fn built_in_extensions_for_session(
    settings: SettingsManager,
    cwd: &std::path::Path,
    agent_dir: &std::path::Path,
) -> Vec<BuiltinExtensionDescriptor> {
    let mut extensions = built_in_extensions(Some(settings.clone()));
    if let Some(mcp) = extensions.iter_mut().find(|e| e.name == "mcp") {
        mcp.factory = create_mcp_extension(McpExtensionOptions {
            agent_dir: Some(agent_dir.to_path_buf()),
            cwd: Some(cwd.to_path_buf()),
            project_trusted: settings.is_project_trusted(),
            ..Default::default()
        });
    }
    extensions
}

pub fn built_in_extensions(
    settings_manager: Option<SettingsManager>,
) -> Vec<BuiltinExtensionDescriptor> {
    vec![
        BuiltinExtensionDescriptor::new(
            "codemode",
            create_codemode_extension(CodemodeExtensionOptions {
                settings_manager,
                ..CodemodeExtensionOptions::default()
            }),
        ),
        BuiltinExtensionDescriptor::new("tool-search", create_tool_search_extension()),
        BuiltinExtensionDescriptor::new(
            "mcp",
            create_mcp_extension(McpExtensionOptions::default()),
        ),
    ]
}

pub mod client;
pub mod config;
mod config_store;
pub mod http;
pub mod oauth;
pub mod protocol;
mod sse;
#[cfg(test)]
mod sse_tests;
pub mod transport;

pub use client::{
    DEFAULT_REQUEST_TIMEOUT_MS, McpClient, McpClientOptions, McpRequestOptions, ProgressCallback,
};
pub use config::{
    LIST_MCP_RESOURCE_TEMPLATES_TOOL, LIST_MCP_RESOURCES_TOOL, LoadedMcpConfig, McpExposure,
    McpHttpServerConfig, McpOAuthConfig, McpProviderAuthConfig, McpServerConfig, McpServerEntry,
    McpStdioServerConfig, READ_MCP_RESOURCE_TOOL, get_mcp_tool_exposure, load_mcp_config,
    load_mcp_config_with_registered, mcp_namespace, validate_mcp_server_config,
};
pub use config_store::{
    add_mcp_server_config, remove_mcp_server_config, set_mcp_server_overrides,
    update_mcp_server_config,
};
pub use oauth::{
    McpOAuthCallback, McpOAuthCallbackServer, McpOAuthChallenge, McpOAuthCredential,
    McpOAuthMetadata, McpOAuthRedirectFn, McpOAuthSession, McpOAuthStore, discover_mcp_oauth,
    generate_mcp_pkce, parse_oauth_challenge,
};
pub use protocol::{
    CallToolResult, ClientCapabilities, Implementation, InitializeResult, JsonRpcId,
    LATEST_PROTOCOL_VERSION, ListResourceTemplatesResult, ListResourcesResult, McpContentBlock,
    McpError, McpResource, McpResourceContents, McpResourceTemplate, McpTool, McpToolAnnotations,
    ProgressNotification, ReadResourceResult, SUPPORTED_PROTOCOL_VERSIONS, ServerCapabilities,
    to_llm_content,
};
pub use transport::{
    InMemoryTransport, McpTransport, SessionReinitializedHandler, StdioTransport,
    StdioTransportOptions, StreamableHttpTransport, StreamableHttpTransportOptions,
};

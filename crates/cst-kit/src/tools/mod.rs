pub mod fff;
pub mod path_guard;
pub mod read;
pub mod ssrf;
pub mod web_access;

use pi_agent::AgentToolResult;
use serde_json::Value;

pub const MAX_OUTPUT_BYTES: usize = 50 * 1024;

pub(crate) fn json_result(value: Value) -> Result<AgentToolResult, String> {
    crate::diagnostics::result::diagnostic_result(value)
}

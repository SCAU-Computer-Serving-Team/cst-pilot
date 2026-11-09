pub mod fff;
pub mod path_guard;
pub mod ssrf;
pub mod web_access;

use pi_agent::AgentToolResult;
use pi_ai::{TextContent, UserContentBlock};
use serde_json::Value;

pub const MAX_OUTPUT_BYTES: usize = 50 * 1024;

pub(crate) fn json_result(value: Value) -> Result<AgentToolResult, String> {
    let text = serde_json::to_string(&value).map_err(|_| "工具结果无法编码。".to_owned())?;
    if text.len() > MAX_OUTPUT_BYTES {
        return Err("工具结果超过 50 KiB，请缩小查询范围。".into());
    }
    Ok(AgentToolResult {
        content: vec![UserContentBlock::Text(TextContent::new(text))],
        details: value,
        structured_content: None,
        usage: None,
        is_error: false,
        terminate: None,
    })
}

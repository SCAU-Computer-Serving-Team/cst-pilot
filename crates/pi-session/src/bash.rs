use std::sync::Arc;

use pi_agent::BoxFuture;
use tokio_util::sync::CancellationToken;

pub type BashChunkCallback = Arc<dyn Fn(String) + Send + Sync>;
pub type BashExecutor =
    Arc<dyn Fn(BashRequest) -> BoxFuture<'static, Result<BashResult, String>> + Send + Sync>;

/// The host supplies local or remote process execution; this crate owns hooks
/// and transcript ordering, without choosing a shell or a process backend.
#[derive(Clone)]
pub struct BashRequest {
    pub command: String,
    pub cwd: String,
    /// Session metadata for the host executor; contains no provider credentials.
    pub env: std::collections::BTreeMap<String, String>,
    pub cancel_token: CancellationToken,
    pub on_chunk: Option<BashChunkCallback>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BashResult {
    pub output: String,
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub truncated: bool,
    pub full_output_path: Option<String>,
}

#[derive(Clone, Default)]
pub struct ExecuteBashOptions {
    pub exclude_from_context: bool,
    pub cancel_token: Option<CancellationToken>,
    pub on_chunk: Option<BashChunkCallback>,
    pub executor: Option<BashExecutor>,
}

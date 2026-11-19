use serde::{Deserialize, Serialize};

use crate::process::ProcessIdentity;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RestartUi {
    Tui,
    Web,
    Desktop,
}

impl RestartUi {
    pub fn argument(self) -> &'static str {
        match self {
            Self::Tui => "tui",
            Self::Web => "web",
            Self::Desktop => "desktop",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LaunchReceipt {
    pub updater_pid: u32,
    pub request_id: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RollbackReason {
    Installation,
    HealthCheck,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateReport {
    pub version: String,
    pub rolled_back: bool,
    pub reason: Option<RollbackReason>,
}

/// Handoff request written by the host into `.state/update/requests/<id>.json`
/// and read by the independent `cst-updater.exe` process.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HandoffRequest {
    pub v: u32,
    pub id: String,
    pub stage: String,
    pub parents: Vec<ProcessIdentity>,
    pub ui: RestartUi,
}

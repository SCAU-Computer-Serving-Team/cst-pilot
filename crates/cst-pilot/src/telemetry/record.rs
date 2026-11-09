use chrono::{Local, SecondsFormat, TimeZone};
use serde::{Deserialize, Serialize};

use super::{billing::BillingSummary, collect::Metrics, environment::Environment};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub v: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub record_id: String,
    pub revision: u64,
    #[serde(rename = "final")]
    pub final_state: bool,
    pub kit_version: String,
    pub session_id: String,
    pub channel: String,
    pub reason: String,
    pub end_reason: String,
    pub started_at: String,
    pub ended_at: String,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_entries: Option<u64>,
    #[serde(flatten)]
    pub environment: Environment,
    #[serde(flatten)]
    pub metrics: Metrics,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cstoa_billing: Option<BillingSummary>,
}
impl SessionRecord {
    pub fn crash(mut self, last_activity: u64) -> Self {
        let started = chrono::DateTime::parse_from_rfc3339(&self.started_at)
            .ok()
            .map(|date| date.timestamp_millis().max(0) as u64)
            .unwrap_or(last_activity);
        let ended = last_activity.max(started);
        self.ended_at = local_iso(ended);
        self.duration_ms = ended - started;
        self.end_reason = "crash".into();
        self.final_state = true;
        self.revision = self.revision.saturating_add(1);
        self
    }
}
pub fn local_iso(at: u64) -> String {
    Local
        .timestamp_millis_opt(at.min(i64::MAX as u64) as i64)
        .single()
        .unwrap_or_default()
        .to_rfc3339_opts(SecondsFormat::Secs, false)
}

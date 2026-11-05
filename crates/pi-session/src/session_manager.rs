use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(feature = "test-support")]
use std::sync::Mutex;
use std::sync::{Arc, OnceLock};

use pi_agent::{AgentMessage, get_agent_current_system_message};
use pi_ai::{
    ContentBlock, JsonValue, SystemMessage, TextContent, Usage, UserContent, UserContentBlock,
    now_ms, uuid_v7,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::messages::{
    BranchSummaryMessage, CompactionSummaryMessage, CustomMessage, CustomMessageContent,
};
use crate::session_store::{
    ClosedSessionStore, InMemorySessionStore, JsonlSessionStore, SessionStore, SessionStoreError,
    TailRepairOutcome, read_session_header,
};

#[cfg(feature = "test-support")]
use crate::session_store::JsonlFaultInjection;

pub const CURRENT_SESSION_VERSION: u32 = 3;
pub const MAX_SESSION_HEADER_SCAN_BYTES: usize = 1024 * 1024;
pub const MAX_CONCURRENT_SESSION_INFO_LOADS: usize = 10;

// =============================================================================
// 1. ISO 8601 Timestamp Utilities (UTC YYYY-MM-DDTHH:MM:SS.sssZ)
// =============================================================================

pub fn format_iso_timestamp(epoch_ms: u64) -> String {
    let total_secs = (epoch_ms / 1000) as i64;
    let millis = (epoch_ms % 1000) as u32;
    let days = total_secs.div_euclid(86_400);
    let secs_of_day = total_secs.rem_euclid(86_400) as u32;
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;

    // Howard Hinnant's civil_from_days algorithm
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!("{year:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

pub fn now_iso_timestamp() -> String {
    format_iso_timestamp(now_ms())
}

pub fn parse_iso_timestamp_ms(iso: &str) -> Option<u64> {
    let s = iso.trim();
    if s.len() < 19
        || s.get(4..5)? != "-"
        || s.get(7..8)? != "-"
        || s.get(10..11)? != "T"
        || s.get(13..14)? != ":"
        || s.get(16..17)? != ":"
    {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    let hour: u32 = s.get(11..13)?.parse().ok()?;
    let minute: u32 = s.get(14..16)?.parse().ok()?;
    let second: u32 = s.get(17..19)?.parse().ok()?;

    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return None,
    };
    if day == 0 || day > days_in_month || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let mut millis: u64 = 0;
    let mut rest = s.get(19..)?;
    if let Some(after_dot) = rest.strip_prefix('.') {
        let digit_count = after_dot.bytes().take_while(u8::is_ascii_digit).count();
        if digit_count == 0 {
            return None;
        }
        let digits = &after_dot[..digit_count.min(3)];
        millis = digits.parse::<u64>().ok()? * 10_u64.pow(3 - digits.len() as u32);
        rest = &after_dot[digit_count..];
    }
    let offset_secs = match rest {
        "" | "Z" => 0,
        offset
            if offset.len() == 6
                && matches!(offset.as_bytes()[0], b'+' | b'-')
                && offset.get(3..4)? == ":" =>
        {
            let hours: i64 = offset.get(1..3)?.parse().ok()?;
            let minutes: i64 = offset.get(4..6)?.parse().ok()?;
            if !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
                return None;
            }
            (hours * 3600 + minutes * 60) * if offset.starts_with('-') { -1 } else { 1 }
        }
        _ => return None,
    };

    // Howard Hinnant's days_from_civil algorithm
    let y = year - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let m_adj = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * m_adj + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + (doe as i64) - 719_468;
    let total_secs = days * 86_400 + i64::from(hour * 3600 + minute * 60 + second) - offset_secs;
    if total_secs < 0 {
        return Some(0);
    }
    Some(
        (total_secs as u64)
            .saturating_mul(1000)
            .saturating_add(millis),
    )
}

// =============================================================================
// 2. SessionHeader & 11 SessionEntry Variants (v3)
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHeader {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    pub id: String,
    pub timestamp: String,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionOptions {
    pub id: Option<String>,
    pub parent_session: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMessageEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub message: AgentMessage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingLevelChangeEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub thinking_level: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelChangeEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub provider: String,
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub kind: String,
    pub provider: String,
    pub model: String,
    pub usage: Usage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub summary: String,
    #[serde(default)]
    pub first_kept_entry_id: String,
    /// 仅供 v1 会话文件反序列化迁移使用，v2/v3 序列化时始终跳过。
    #[serde(default, skip_serializing)]
    pub first_kept_entry_index: Option<usize>,
    pub tokens_before: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_hook: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_message: Option<SystemMessage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub from_id: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_hook: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub custom_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<JsonValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub target_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfoEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessageEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub custom_type: String,
    pub content: CustomMessageContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
    pub display: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEditReplacement {
    pub content: JsonValue,
}

impl ContextEditReplacement {
    pub fn from_text(text: impl Into<String>) -> Self {
        Self {
            content: Value::String(text.into()),
        }
    }

    pub fn from_value(content: JsonValue) -> Self {
        Self { content }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEditEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub target_id: String,
    pub replacement: Option<ContextEditReplacement>,
}

/// 会话追加树的 11 种条目枚举（对照 `SessionEntry`）。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEntry {
    Message(SessionMessageEntry),
    ThinkingLevelChange(ThinkingLevelChangeEntry),
    ModelChange(ModelChangeEntry),
    Usage(UsageEntry),
    Compaction(CompactionEntry),
    BranchSummary(BranchSummaryEntry),
    Custom(CustomEntry),
    CustomMessage(CustomMessageEntry),
    ContextEdit(ContextEditEntry),
    Label(LabelEntry),
    SessionInfo(SessionInfoEntry),
}

impl SessionEntry {
    pub fn id(&self) -> &str {
        match self {
            SessionEntry::Message(e) => &e.id,
            SessionEntry::ThinkingLevelChange(e) => &e.id,
            SessionEntry::ModelChange(e) => &e.id,
            SessionEntry::Usage(e) => &e.id,
            SessionEntry::Compaction(e) => &e.id,
            SessionEntry::BranchSummary(e) => &e.id,
            SessionEntry::Custom(e) => &e.id,
            SessionEntry::CustomMessage(e) => &e.id,
            SessionEntry::ContextEdit(e) => &e.id,
            SessionEntry::Label(e) => &e.id,
            SessionEntry::SessionInfo(e) => &e.id,
        }
    }

    pub fn set_id(&mut self, id: String) {
        match self {
            SessionEntry::Message(e) => e.id = id,
            SessionEntry::ThinkingLevelChange(e) => e.id = id,
            SessionEntry::ModelChange(e) => e.id = id,
            SessionEntry::Usage(e) => e.id = id,
            SessionEntry::Compaction(e) => e.id = id,
            SessionEntry::BranchSummary(e) => e.id = id,
            SessionEntry::Custom(e) => e.id = id,
            SessionEntry::CustomMessage(e) => e.id = id,
            SessionEntry::ContextEdit(e) => e.id = id,
            SessionEntry::Label(e) => e.id = id,
            SessionEntry::SessionInfo(e) => e.id = id,
        }
    }

    pub fn parent_id(&self) -> Option<&str> {
        match self {
            SessionEntry::Message(e) => e.parent_id.as_deref(),
            SessionEntry::ThinkingLevelChange(e) => e.parent_id.as_deref(),
            SessionEntry::ModelChange(e) => e.parent_id.as_deref(),
            SessionEntry::Usage(e) => e.parent_id.as_deref(),
            SessionEntry::Compaction(e) => e.parent_id.as_deref(),
            SessionEntry::BranchSummary(e) => e.parent_id.as_deref(),
            SessionEntry::Custom(e) => e.parent_id.as_deref(),
            SessionEntry::CustomMessage(e) => e.parent_id.as_deref(),
            SessionEntry::ContextEdit(e) => e.parent_id.as_deref(),
            SessionEntry::Label(e) => e.parent_id.as_deref(),
            SessionEntry::SessionInfo(e) => e.parent_id.as_deref(),
        }
    }

    pub fn set_parent_id(&mut self, parent_id: Option<String>) {
        match self {
            SessionEntry::Message(e) => e.parent_id = parent_id,
            SessionEntry::ThinkingLevelChange(e) => e.parent_id = parent_id,
            SessionEntry::ModelChange(e) => e.parent_id = parent_id,
            SessionEntry::Usage(e) => e.parent_id = parent_id,
            SessionEntry::Compaction(e) => e.parent_id = parent_id,
            SessionEntry::BranchSummary(e) => e.parent_id = parent_id,
            SessionEntry::Custom(e) => e.parent_id = parent_id,
            SessionEntry::CustomMessage(e) => e.parent_id = parent_id,
            SessionEntry::ContextEdit(e) => e.parent_id = parent_id,
            SessionEntry::Label(e) => e.parent_id = parent_id,
            SessionEntry::SessionInfo(e) => e.parent_id = parent_id,
        }
    }

    pub fn timestamp(&self) -> &str {
        match self {
            SessionEntry::Message(e) => &e.timestamp,
            SessionEntry::ThinkingLevelChange(e) => &e.timestamp,
            SessionEntry::ModelChange(e) => &e.timestamp,
            SessionEntry::Usage(e) => &e.timestamp,
            SessionEntry::Compaction(e) => &e.timestamp,
            SessionEntry::BranchSummary(e) => &e.timestamp,
            SessionEntry::Custom(e) => &e.timestamp,
            SessionEntry::CustomMessage(e) => &e.timestamp,
            SessionEntry::ContextEdit(e) => &e.timestamp,
            SessionEntry::Label(e) => &e.timestamp,
            SessionEntry::SessionInfo(e) => &e.timestamp,
        }
    }

    pub fn entry_type(&self) -> &'static str {
        match self {
            SessionEntry::Message(_) => "message",
            SessionEntry::ThinkingLevelChange(_) => "thinking_level_change",
            SessionEntry::ModelChange(_) => "model_change",
            SessionEntry::Usage(_) => "usage",
            SessionEntry::Compaction(_) => "compaction",
            SessionEntry::BranchSummary(_) => "branch_summary",
            SessionEntry::Custom(_) => "custom",
            SessionEntry::CustomMessage(_) => "custom_message",
            SessionEntry::ContextEdit(_) => "context_edit",
            SessionEntry::Label(_) => "label",
            SessionEntry::SessionInfo(_) => "session_info",
        }
    }
}

/// 原始文件行（头记录或会话条目）。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum FileEntry {
    Session(SessionHeader),
    Entry(SessionEntry),
}

impl Serialize for FileEntry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            FileEntry::Session(h) => {
                let mut val = serde_json::to_value(h).map_err(serde::ser::Error::custom)?;
                if let Value::Object(map) = &mut val {
                    map.insert("type".to_owned(), Value::String("session".to_owned()));
                }
                val.serialize(serializer)
            }
            FileEntry::Entry(e) => e.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for FileEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mut val = Value::deserialize(deserializer)?;
        let Some(obj) = val.as_object_mut() else {
            return Err(serde::de::Error::custom("FileEntry must be a JSON object"));
        };
        let Some(entry_type) = obj
            .get("type")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
        else {
            return Err(serde::de::Error::custom("FileEntry missing string 'type'"));
        };
        if entry_type == "session" {
            let header: SessionHeader =
                serde_json::from_value(val).map_err(serde::de::Error::custom)?;
            Ok(FileEntry::Session(header))
        } else {
            // 修复手工编辑或旧版文件中 message.content == null 的情况
            if entry_type == "message"
                && let Some(Value::Object(msg_obj)) = obj.get_mut("message")
                && msg_obj.get("content").is_none_or(Value::is_null)
            {
                let role = msg_obj
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if role == "system" {
                    msg_obj.insert("content".to_owned(), Value::String(String::new()));
                } else if matches!(role, "user" | "assistant" | "toolResult") {
                    msg_obj.insert("content".to_owned(), Value::Array(Vec::new()));
                }
            }
            let entry: SessionEntry =
                serde_json::from_value(val).map_err(serde::de::Error::custom)?;
            Ok(FileEntry::Entry(entry))
        }
    }
}

pub fn parse_session_entry_line(line: &str) -> Option<FileEntry> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str::<FileEntry>(trimmed).ok()
}

pub fn parse_session_entries(content: &str) -> Vec<FileEntry> {
    content
        .lines()
        .filter_map(parse_session_entry_line)
        .collect()
}

// =============================================================================
// 3. Session ID Validation, Short ID Generation & v1->v2->v3 Migration
// =============================================================================

fn session_id_regex() -> &'static Regex {
    static LOCK: OnceLock<Regex> = OnceLock::new();
    LOCK.get_or_init(|| {
        Regex::new(r"^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$").expect("valid session_id regex")
    })
}

pub fn assert_valid_session_id(id: &str) -> Result<(), SessionStoreError> {
    if !session_id_regex().is_match(id) {
        return Err(SessionStoreError::InvalidOperation(
            "Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character".to_owned(),
        ));
    }
    Ok(())
}

pub fn generate_short_id(contains: impl Fn(&str) -> bool) -> String {
    for _ in 0..100 {
        let full = Uuid::new_v4().to_string();
        let short = &full[..8];
        if !contains(short) {
            return short.to_owned();
        }
    }
    Uuid::new_v4().to_string()
}

fn migrate_v1_to_v2(entries: &mut [FileEntry]) {
    let mut ids: BTreeSet<String> = BTreeSet::new();
    let mut prev_id: Option<String> = None;

    for entry in entries.iter_mut() {
        match entry {
            FileEntry::Session(header) => {
                header.version = Some(2);
            }
            FileEntry::Entry(session_entry) => {
                let new_id = generate_short_id(|candidate| ids.contains(candidate));
                ids.insert(new_id.clone());
                session_entry.set_id(new_id.clone());
                session_entry.set_parent_id(prev_id);
                prev_id = Some(new_id);
            }
        }
    }

    // 第二遍：将 CompactionEntry 上的 first_kept_entry_index 转换为 first_kept_entry_id
    let id_by_index: Vec<Option<String>> = entries
        .iter()
        .map(|e| match e {
            FileEntry::Session(_) => None,
            FileEntry::Entry(se) => Some(se.id().to_owned()),
        })
        .collect();

    for entry in entries.iter_mut() {
        if let FileEntry::Entry(SessionEntry::Compaction(comp)) = entry
            && let Some(idx) = comp.first_kept_entry_index.take()
            && let Some(Some(target_id)) = id_by_index.get(idx)
        {
            comp.first_kept_entry_id = target_id.clone();
        }
    }
}

fn migrate_v2_to_v3(entries: &mut [FileEntry]) {
    for entry in entries.iter_mut() {
        match entry {
            FileEntry::Session(header) => {
                header.version = Some(3);
            }
            FileEntry::Entry(SessionEntry::Message(msg_entry)) => {
                if let AgentMessage::Custom(custom) = &mut msg_entry.message
                    && custom.role == "hookMessage"
                {
                    custom.role = "custom".to_owned();
                }
            }
            _ => {}
        }
    }
}

/// 将条目原地升级到 `CURRENT_SESSION_VERSION (3)`；若有发生迁移则返回 `true`。
pub fn migrate_to_current_version(entries: &mut [FileEntry]) -> bool {
    let version = entries
        .iter()
        .find_map(|e| match e {
            FileEntry::Session(h) => Some(h.version.unwrap_or(1)),
            _ => None,
        })
        .unwrap_or(1);

    if version >= CURRENT_SESSION_VERSION {
        return false;
    }
    if version < 2 {
        migrate_v1_to_v2(entries);
    }
    if version < 3 {
        migrate_v2_to_v3(entries);
    }
    true
}

pub fn migrate_session_entries(entries: &mut [FileEntry]) {
    let _ = migrate_to_current_version(entries);
}

// =============================================================================
// 4. Context Projection & Dual-View Builders
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModelRef {
    pub provider: String,
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectedSessionEntry {
    pub source_entry: SessionEntry,
    pub messages: Vec<AgentMessage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionProjection {
    pub entries: Vec<ProjectedSessionEntry>,
    pub messages: Vec<AgentMessage>,
    pub thinking_level: String,
    pub model: Option<SessionModelRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionContext {
    pub messages: Vec<AgentMessage>,
    pub thinking_level: String,
    pub model: Option<SessionModelRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTreeNode {
    pub entry: SessionEntry,
    pub children: Vec<SessionTreeNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_timestamp: Option<String>,
}

impl Drop for SessionTreeNode {
    fn drop(&mut self) {
        // A session can contain a very deep, valid parent chain. Drain descendants
        // iteratively so releasing the returned tree cannot overflow the thread stack.
        let mut pending = std::mem::take(&mut self.children);
        while let Some(mut node) = pending.pop() {
            pending.append(&mut node.children);
        }
    }
}

/// 边界事件（`turn_end` / `agent_before_settle`）返回的待原子提交草案（对照 `SessionBoundaryDraft`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionBoundaryDraft {
    #[serde(rename_all = "camelCase")]
    Custom {
        custom_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<JsonValue>,
    },
    #[serde(rename_all = "camelCase")]
    CustomMessage {
        custom_type: String,
        content: CustomMessageContent,
        #[serde(skip_serializing_if = "Option::is_none")]
        display: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<JsonValue>,
    },
    #[serde(rename_all = "camelCase")]
    ContextEdit {
        target_id: String,
        replacement: Option<ContextEditReplacement>,
    },
    #[serde(rename_all = "camelCase")]
    Compaction {
        summary: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        first_kept_entry_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tokens_before: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<JsonValue>,
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(skip_serializing_if = "Option::is_none")]
        from_hook: Option<bool>,
    },
}

pub fn get_latest_compaction_entry(entries: &[SessionEntry]) -> Option<&CompactionEntry> {
    entries.iter().rev().find_map(|e| match e {
        SessionEntry::Compaction(c) => Some(c),
        _ => None,
    })
}

pub fn session_entry_to_context_messages(entry: &SessionEntry) -> Vec<AgentMessage> {
    match entry {
        SessionEntry::Message(m) => vec![m.message.clone()],
        SessionEntry::CustomMessage(cm) => {
            let ts = parse_iso_timestamp_ms(&cm.timestamp).unwrap_or(0);
            let msg = CustomMessage {
                custom_type: cm.custom_type.clone(),
                content: cm.content.clone(),
                display: cm.display,
                details: cm.details.clone(),
                timestamp: ts,
            };
            vec![msg.to_agent_message()]
        }
        SessionEntry::BranchSummary(bs) => {
            if bs.summary.is_empty() {
                Vec::new()
            } else {
                let ts = parse_iso_timestamp_ms(&bs.timestamp).unwrap_or(0);
                let msg = BranchSummaryMessage {
                    summary: bs.summary.clone(),
                    from_id: Some(bs.from_id.clone()),
                    timestamp: ts,
                };
                vec![msg.to_agent_message()]
            }
        }
        SessionEntry::Compaction(c) => {
            let ts = parse_iso_timestamp_ms(&c.timestamp).unwrap_or(0);
            let summary_msg = CompactionSummaryMessage {
                summary: c.summary.clone(),
                tokens_before: c.tokens_before,
                timestamp: ts,
            }
            .to_agent_message();
            if let Some(sys) = &c.system_message {
                vec![AgentMessage::System(sys.clone()), summary_msg]
            } else {
                vec![summary_msg]
            }
        }
        _ => Vec::new(),
    }
}

pub fn project_context_entry(
    entry: &SessionEntry,
    edit: Option<&ContextEditEntry>,
) -> Vec<AgentMessage> {
    let messages = session_entry_to_context_messages(entry);
    let Some(edit) = edit else {
        return messages;
    };
    let Some(replacement) = &edit.replacement else {
        return Vec::new();
    };

    messages
        .into_iter()
        .map(|msg| match msg {
            AgentMessage::User(mut u) => {
                match &replacement.content {
                    Value::String(s) => u.content = UserContent::Text(s.clone()),
                    Value::Array(arr) => {
                        if let Ok(blocks) = serde_json::from_value::<Vec<UserContentBlock>>(
                            Value::Array(arr.clone()),
                        ) {
                            u.content = UserContent::Blocks(blocks);
                        }
                    }
                    _ => {}
                }
                AgentMessage::User(u)
            }
            AgentMessage::Assistant(mut a) => {
                match &replacement.content {
                    Value::String(s) => {
                        a.content = vec![ContentBlock::Text(TextContent::new(s.clone()))];
                    }
                    Value::Array(arr) => {
                        if let Ok(blocks) =
                            serde_json::from_value::<Vec<ContentBlock>>(Value::Array(arr.clone()))
                        {
                            a.content = blocks;
                        }
                    }
                    _ => {}
                }
                AgentMessage::Assistant(a)
            }
            AgentMessage::ToolResult(mut tr) => {
                match &replacement.content {
                    Value::String(s) => {
                        tr.content = vec![UserContentBlock::Text(TextContent::new(s.clone()))];
                    }
                    Value::Array(arr) => {
                        if let Ok(blocks) = serde_json::from_value::<Vec<UserContentBlock>>(
                            Value::Array(arr.clone()),
                        ) {
                            tr.content = blocks;
                        }
                    }
                    _ => {}
                }
                AgentMessage::ToolResult(tr)
            }
            AgentMessage::Custom(mut c) if c.role == "custom" => {
                c.data
                    .insert("content".to_owned(), replacement.content.clone());
                AgentMessage::Custom(c)
            }
            other => other,
        })
        .collect()
}

fn build_session_path(
    entries: &[SessionEntry],
    leaf_id: Option<Option<&str>>,
    by_id: Option<&BTreeMap<String, SessionEntry>>,
) -> Vec<SessionEntry> {
    let owned_map;
    let index = if let Some(map) = by_id {
        map
    } else {
        owned_map = entries
            .iter()
            .map(|e| (e.id().to_owned(), e.clone()))
            .collect::<BTreeMap<String, SessionEntry>>();
        &owned_map
    };

    let leaf: Option<&SessionEntry> = match leaf_id {
        Some(None) => return Vec::new(),
        Some(Some(id)) => index.get(id).or_else(|| entries.last()),
        None => entries.last(),
    };
    let Some(start) = leaf else {
        return Vec::new();
    };

    let mut path = Vec::new();
    let mut current: Option<&SessionEntry> = Some(start);
    let mut visited = BTreeSet::new();
    while let Some(node) = current {
        if !visited.insert(node.id().to_owned()) {
            break;
        }
        path.push(node.clone());
        current = node.parent_id().and_then(|pid| index.get(pid));
    }
    path.reverse();
    path
}

fn get_session_context_settings(path: &[SessionEntry]) -> (String, Option<SessionModelRef>) {
    let mut thinking_level = "off".to_owned();
    let mut model: Option<SessionModelRef> = None;

    for entry in path {
        match entry {
            SessionEntry::ThinkingLevelChange(tl) => {
                thinking_level = tl.thinking_level.clone();
            }
            SessionEntry::ModelChange(mc) => {
                model = Some(SessionModelRef {
                    provider: mc.provider.clone(),
                    model_id: mc.model_id.clone(),
                });
            }
            SessionEntry::Message(m) => {
                if let AgentMessage::Assistant(a) = &m.message {
                    model = Some(SessionModelRef {
                        provider: a.provider.clone(),
                        model_id: a.model.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    (thinking_level, model)
}

/// 构建当前分支经过压缩裁剪后的上下文条目列表（对照 `buildContextEntries`）。
pub fn build_context_entries(
    entries: &[SessionEntry],
    leaf_id: Option<Option<&str>>,
    by_id: Option<&BTreeMap<String, SessionEntry>>,
) -> Vec<SessionEntry> {
    let path = build_session_path(entries, leaf_id, by_id);
    let mut latest_compaction_idx: Option<usize> = None;
    for (idx, entry) in path.iter().enumerate() {
        if matches!(entry, SessionEntry::Compaction(_)) {
            latest_compaction_idx = Some(idx);
        }
    }

    let Some(compaction_idx) = latest_compaction_idx else {
        return path;
    };
    let SessionEntry::Compaction(compaction) = &path[compaction_idx] else {
        return path;
    };

    let mut context_entries = Vec::with_capacity(path.len());
    context_entries.push(path[compaction_idx].clone());

    let mut found_first_kept = false;
    for entry in &path[..compaction_idx] {
        if entry.id() == compaction.first_kept_entry_id {
            found_first_kept = true;
        }
        let is_system_msg =
            matches!(entry, SessionEntry::Message(m) if m.message.role() == "system");
        if found_first_kept && !is_system_msg {
            context_entries.push(entry.clone());
        }
    }
    context_entries.extend_from_slice(&path[compaction_idx + 1..]);
    context_entries
}

/// 构建保留源条目溯源与 `ContextEdit` 修饰的模型上下文投影（对照 `buildSessionProjection`）。
pub fn build_session_projection(
    entries: &[SessionEntry],
    leaf_id: Option<Option<&str>>,
    by_id: Option<&BTreeMap<String, SessionEntry>>,
) -> SessionProjection {
    let path = build_session_path(entries, leaf_id, by_id);
    let (thinking_level, model) = get_session_context_settings(&path);
    let context_entries = build_context_entries(entries, leaf_id, by_id);

    let mut edits: BTreeMap<String, ContextEditEntry> = BTreeMap::new();
    for entry in &context_entries {
        if let SessionEntry::ContextEdit(ce) = entry {
            edits.insert(ce.target_id.clone(), ce.clone());
        }
    }

    let projected_entries: Vec<ProjectedSessionEntry> = context_entries
        .into_iter()
        .enumerate()
        .map(|(index, source_entry)| {
            // 仅下标 0 的最新 CompactionEntry 展开为检查点与总结消息；
            // 保留区间内夹带的更早 CompactionEntry 投影为空。
            let messages = if matches!(source_entry, SessionEntry::Compaction(_)) && index > 0 {
                Vec::new()
            } else {
                let edit = edits.get(source_entry.id());
                project_context_entry(&source_entry, edit)
            };
            ProjectedSessionEntry {
                source_entry,
                messages,
            }
        })
        .collect();

    let messages = projected_entries
        .iter()
        .flat_map(|e| e.messages.clone())
        .collect();

    SessionProjection {
        entries: projected_entries,
        messages,
        thinking_level,
        model,
    }
}

/// 构建最终发往大模型的会话上下文（对照 `buildSessionContext`）。
pub fn build_session_context(
    entries: &[SessionEntry],
    leaf_id: Option<Option<&str>>,
    by_id: Option<&BTreeMap<String, SessionEntry>>,
) -> SessionContext {
    let proj = build_session_projection(entries, leaf_id, by_id);
    SessionContext {
        messages: proj.messages,
        thinking_level: proj.thinking_level,
        model: proj.model,
    }
}

// =============================================================================
// 5. SessionManager
// =============================================================================

#[derive(Debug, Clone, Default)]
struct SessionMemoryState {
    session_id: String,
    file_entries: Vec<FileEntry>,
    by_id: BTreeMap<String, SessionEntry>,
    labels_by_id: BTreeMap<String, String>,
    label_timestamps_by_id: BTreeMap<String, String>,
    leaf_id: Option<String>,
    flushed: bool,
}

impl SessionMemoryState {
    fn has_conversation(&self) -> bool {
        self.file_entries.iter().any(|e| {
            matches!(
                e,
                FileEntry::Entry(SessionEntry::Message(m))
                    if m.message.role() == "user" || m.message.role() == "assistant"
            )
        })
    }

    fn rebuild_index(&mut self) {
        self.by_id.clear();
        self.labels_by_id.clear();
        self.label_timestamps_by_id.clear();
        self.leaf_id = None;

        for entry in &self.file_entries {
            let FileEntry::Entry(se) = entry else {
                continue;
            };
            self.by_id.insert(se.id().to_owned(), se.clone());
            self.leaf_id = Some(se.id().to_owned());
            if let SessionEntry::Label(le) = se {
                if let Some(lbl) = &le.label
                    && !lbl.is_empty()
                {
                    self.labels_by_id.insert(le.target_id.clone(), lbl.clone());
                    self.label_timestamps_by_id
                        .insert(le.target_id.clone(), le.timestamp.clone());
                } else {
                    self.labels_by_id.remove(&le.target_id);
                    self.label_timestamps_by_id.remove(&le.target_id);
                }
            }
        }
    }

    fn next_id(&self) -> String {
        generate_short_id(|id| self.by_id.contains_key(id))
    }

    fn entries(&self) -> Vec<SessionEntry> {
        self.file_entries
            .iter()
            .filter_map(|e| match e {
                FileEntry::Session(_) => None,
                FileEntry::Entry(se) => Some(se.clone()),
            })
            .collect()
    }

    fn get_branch(&self, from_id: Option<&str>) -> Vec<SessionEntry> {
        let start_id = from_id.or(self.leaf_id.as_deref());
        let Some(mut current) = start_id.and_then(|id| self.by_id.get(id)) else {
            return Vec::new();
        };
        let mut path = Vec::new();
        let mut visited = BTreeSet::new();
        loop {
            if !visited.insert(current.id().to_owned()) {
                break;
            }
            path.push(current.clone());
            match current.parent_id().and_then(|pid| self.by_id.get(pid)) {
                Some(parent) => current = parent,
                None => break,
            }
        }
        path.reverse();
        path
    }

    fn validate_and_create_context_edit(
        &self,
        target_id: &str,
        replacement: Option<ContextEditReplacement>,
    ) -> Result<SessionEntry, SessionStoreError> {
        if let Some(rep) = &replacement
            && !rep.content.is_string()
            && !rep.content.is_array()
        {
            return Err(SessionStoreError::InvalidOperation(
                "Context edit replacement must be null or contain string/array content".to_owned(),
            ));
        }

        let Some(target) = self.by_id.get(target_id) else {
            return Err(SessionStoreError::InvalidOperation(format!(
                "Entry {target_id} not found"
            )));
        };

        let on_active_branch = self
            .get_branch(None)
            .iter()
            .any(|entry| entry.id() == target_id);
        if !on_active_branch {
            return Err(SessionStoreError::InvalidOperation(format!(
                "Entry {target_id} is not on the active branch"
            )));
        }

        let target_role = match target {
            SessionEntry::CustomMessage(_) => "custom",
            SessionEntry::Message(m)
                if matches!(m.message.role(), "user" | "assistant" | "toolResult") =>
            {
                m.message.role()
            }
            _ => {
                return Err(SessionStoreError::InvalidOperation(format!(
                    "Entry {target_id} does not contribute editable model content"
                )));
            }
        };

        let normalized_replacement = replacement.map(|rep| {
            if (target_role == "assistant" || target_role == "toolResult")
                && let Some(text) = rep.content.as_str()
            {
                ContextEditReplacement {
                    content: serde_json::json!([{ "type": "text", "text": text }]),
                }
            } else {
                rep
            }
        });

        if let Some(rep) = &normalized_replacement
            && rep.content.is_array()
        {
            let valid = if target_role == "assistant" {
                serde_json::from_value::<Vec<ContentBlock>>(rep.content.clone()).is_ok()
            } else {
                serde_json::from_value::<Vec<UserContentBlock>>(rep.content.clone()).is_ok()
            };
            if !valid {
                return Err(SessionStoreError::InvalidOperation(
                    "Invalid context edit content blocks".to_owned(),
                ));
            }
        }
        Ok(SessionEntry::ContextEdit(ContextEditEntry {
            id: self.next_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_iso_timestamp(),
            target_id: target_id.to_owned(),
            replacement: normalized_replacement,
        }))
    }

    fn create_compaction_entry(
        &self,
        summary: String,
        first_kept_entry_id: Option<String>,
        tokens_before: u64,
        details: Option<JsonValue>,
        from_hook: Option<bool>,
        usage: Option<Usage>,
    ) -> Result<SessionEntry, SessionStoreError> {
        if let Some(anchor) = &first_kept_entry_id
            && !self
                .get_branch(None)
                .iter()
                .any(|entry| entry.id() == anchor)
        {
            return Err(SessionStoreError::InvalidOperation(format!(
                "Compaction anchor {anchor} is not on the active branch"
            )));
        }
        let timestamp = now_iso_timestamp();
        let entries = self.entries();
        let proj =
            build_session_projection(&entries, Some(self.leaf_id.as_deref()), Some(&self.by_id));
        let mut sys_msg = get_agent_current_system_message(&proj.messages);
        if let Some(sys) = &mut sys_msg {
            sys.timestamp = parse_iso_timestamp_ms(&timestamp).unwrap_or_else(now_ms);
        }
        let id = self.next_id();
        let first_kept = first_kept_entry_id.unwrap_or_else(|| id.clone());
        Ok(SessionEntry::Compaction(CompactionEntry {
            id,
            parent_id: self.leaf_id.clone(),
            timestamp,
            summary,
            first_kept_entry_id: first_kept,
            first_kept_entry_index: None,
            tokens_before,
            details,
            usage,
            from_hook,
            system_message: sys_msg,
        }))
    }

    fn push_entry_in_memory(&mut self, entry: SessionEntry) {
        let id = entry.id().to_owned();
        if let SessionEntry::Label(le) = &entry {
            if let Some(lbl) = &le.label
                && !lbl.is_empty()
            {
                self.labels_by_id.insert(le.target_id.clone(), lbl.clone());
                self.label_timestamps_by_id
                    .insert(le.target_id.clone(), le.timestamp.clone());
            } else {
                self.labels_by_id.remove(&le.target_id);
                self.label_timestamps_by_id.remove(&le.target_id);
            }
        }
        self.leaf_id = Some(id.clone());
        self.by_id.insert(id, entry.clone());
        self.file_entries.push(FileEntry::Entry(entry));
    }
}

/// 会话追加树与持久化控制器（对照上游 `SessionManager`）。
pub struct SessionManager {
    cwd: String,
    session_dir: PathBuf,
    session_file: Option<PathBuf>,
    store: Box<dyn SessionStore>,
    #[cfg(feature = "test-support")]
    fault_injection: Option<Arc<Mutex<JsonlFaultInjection>>>,
    state: SessionMemoryState,
}

impl std::fmt::Debug for SessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionManager")
            .field("session_id", &self.state.session_id)
            .field("cwd", &self.cwd)
            .field("session_file", &self.session_file)
            .field("flushed", &self.state.flushed)
            .field("entry_count", &self.state.by_id.len())
            .finish()
    }
}

impl SessionManager {
    /// Open an application-supplied backend. The backend owns its single-writer
    /// lease and must retain it until dropped. Empty backends remain lazy until
    /// the first conversation message; migrations are committed before return.
    pub fn from_store(
        cwd: impl Into<String>,
        mut store: Box<dyn SessionStore>,
        options: Option<NewSessionOptions>,
    ) -> Result<Self, SessionStoreError> {
        if store.is_poisoned() {
            return Err(SessionStoreError::Poisoned(
                "Session store is poisoned".to_owned(),
            ));
        }
        let cwd = cwd.into();
        let options = options.unwrap_or_default();
        if let Some(id) = &options.id {
            assert_valid_session_id(id)?;
        }
        let mut entries = store.load_entries()?;
        let loaded = !entries.is_empty();
        let session_id = if loaded {
            let Some(FileEntry::Session(header)) = entries.first() else {
                return Err(SessionStoreError::InvalidSessionFile(
                    "Missing session header".to_owned(),
                ));
            };
            assert_valid_session_id(&header.id)?;
            let id = header.id.clone();
            if migrate_to_current_version(&mut entries) {
                store.rewrite_all(&entries, false)?;
            }
            id
        } else {
            let id = options.id.unwrap_or_else(uuid_v7);
            entries.push(FileEntry::Session(SessionHeader {
                version: Some(CURRENT_SESSION_VERSION),
                id: id.clone(),
                timestamp: now_iso_timestamp(),
                cwd: cwd.clone(),
                parent_session: options.parent_session,
            }));
            id
        };
        if !loaded && !store.is_persisted() {
            store.rewrite_all(&entries, false)?;
        }
        let session_file = store.location().map(Path::to_path_buf);
        let session_dir = session_file
            .as_ref()
            .and_then(|path| path.parent())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let mut state = SessionMemoryState {
            session_id,
            file_entries: entries,
            flushed: loaded && store.is_persisted(),
            ..Default::default()
        };
        state.rebuild_index();
        Ok(Self {
            cwd,
            session_dir,
            session_file,
            store,
            #[cfg(feature = "test-support")]
            fault_injection: None,
            state,
        })
    }

    /// 创建持久化会话（惰性创建 `.jsonl` 文件，在出现首条 `user`/`assistant` 消息前不落盘）。
    pub fn create(
        cwd: impl Into<String>,
        session_dir: impl Into<PathBuf>,
        options: Option<NewSessionOptions>,
    ) -> Result<Self, SessionStoreError> {
        let cwd = cwd.into();
        let session_dir = session_dir.into();
        if !session_dir.as_os_str().is_empty() && !session_dir.exists() {
            fs::create_dir_all(&session_dir).map_err(|e| {
                SessionStoreError::Io(format!(
                    "Failed to create session dir {}: {e}",
                    session_dir.display()
                ))
            })?;
        }

        let opts = options.unwrap_or_default();
        if let Some(id) = &opts.id {
            assert_valid_session_id(id)?;
        }
        let session_id = opts.id.unwrap_or_else(uuid_v7);
        let timestamp = now_iso_timestamp();
        let file_ts = timestamp.replace([':', '.'], "-");
        let session_file = session_dir.join(format!("{file_ts}_{session_id}.jsonl"));
        let jsonl_store = JsonlSessionStore::open(&session_file)?;
        #[cfg(feature = "test-support")]
        let fault = jsonl_store.fault_injection();

        let header = SessionHeader {
            version: Some(CURRENT_SESSION_VERSION),
            id: session_id.clone(),
            timestamp,
            cwd: cwd.clone(),
            parent_session: opts.parent_session,
        };

        Ok(Self {
            cwd,
            session_dir,
            session_file: Some(session_file),
            store: Box::new(jsonl_store),
            #[cfg(feature = "test-support")]
            fault_injection: Some(fault),
            state: SessionMemoryState {
                session_id,
                file_entries: vec![FileEntry::Session(header)],
                by_id: BTreeMap::new(),
                labels_by_id: BTreeMap::new(),
                label_timestamps_by_id: BTreeMap::new(),
                leaf_id: None,
                flushed: false,
            },
        })
    }

    /// 打开指定路径的会话文件（自动校验头部、修复尾部、执行 `v1 -> v2 -> v3` 迁移并持有单写权）。
    pub fn open(
        path: impl Into<PathBuf>,
        session_dir: Option<PathBuf>,
        cwd_override: Option<String>,
    ) -> Result<Self, SessionStoreError> {
        Self::open_inner(path.into(), session_dir, cwd_override, true)
    }

    /// Open an existing, nonempty session. Validation occurs after acquiring its write lock.
    pub fn open_existing(
        path: impl Into<PathBuf>,
        session_dir: Option<PathBuf>,
        cwd_override: Option<String>,
    ) -> Result<Self, SessionStoreError> {
        Self::open_inner(path.into(), session_dir, cwd_override, false)
    }

    fn open_inner(
        resolved_path: PathBuf,
        session_dir: Option<PathBuf>,
        cwd_override: Option<String>,
        allow_create: bool,
    ) -> Result<Self, SessionStoreError> {
        let dir = session_dir.unwrap_or_else(|| {
            resolved_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."))
        });

        let mut jsonl_store = JsonlSessionStore::open(&resolved_path)?;
        #[cfg(feature = "test-support")]
        let fault = jsonl_store.fault_injection();
        if !allow_create
            && fs::metadata(&resolved_path).map_or(true, |m| !m.is_file() || m.len() == 0)
        {
            return Err(SessionStoreError::InvalidSessionFile(
                resolved_path.display().to_string(),
            ));
        }

        if resolved_path.exists() {
            let file_size = fs::metadata(&resolved_path)
                .map_err(|e| SessionStoreError::Io(e.to_string()))?
                .len();
            let mut entries = jsonl_store.load_entries()?;
            if entries.is_empty() {
                if file_size > 0 {
                    return Err(SessionStoreError::InvalidSessionFile(
                        resolved_path.display().to_string(),
                    ));
                }
                // 0 字节空文件：初始化合法 SessionHeader 并落盘
                let session_id = uuid_v7();
                let timestamp = now_iso_timestamp();
                let cwd = cwd_override.unwrap_or_else(|| {
                    std::env::current_dir()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| ".".to_owned())
                });
                let header = SessionHeader {
                    version: Some(CURRENT_SESSION_VERSION),
                    id: session_id.clone(),
                    timestamp,
                    cwd: cwd.clone(),
                    parent_session: None,
                };
                let file_entries = vec![FileEntry::Session(header)];
                jsonl_store.rewrite_all(&file_entries, false)?;
                return Ok(Self {
                    cwd,
                    session_dir: dir,
                    session_file: Some(resolved_path),
                    store: Box::new(jsonl_store),
                    #[cfg(feature = "test-support")]
                    fault_injection: Some(fault),
                    state: SessionMemoryState {
                        session_id,
                        file_entries,
                        by_id: BTreeMap::new(),
                        labels_by_id: BTreeMap::new(),
                        label_timestamps_by_id: BTreeMap::new(),
                        leaf_id: None,
                        flushed: true,
                    },
                });
            }

            let header = entries
                .iter()
                .find_map(|e| match e {
                    FileEntry::Session(h) => Some(h.clone()),
                    _ => None,
                })
                .ok_or_else(|| {
                    SessionStoreError::InvalidSessionFile(resolved_path.display().to_string())
                })?;

            if migrate_to_current_version(&mut entries) {
                jsonl_store.rewrite_all(&entries, false)?;
            }

            let cwd = cwd_override.unwrap_or_else(|| header.cwd.clone());
            let mut state = SessionMemoryState {
                session_id: header.id,
                file_entries: entries,
                by_id: BTreeMap::new(),
                labels_by_id: BTreeMap::new(),
                label_timestamps_by_id: BTreeMap::new(),
                leaf_id: None,
                flushed: true,
            };
            state.rebuild_index();

            Ok(Self {
                cwd,
                session_dir: dir,
                session_file: Some(resolved_path),
                store: Box::new(jsonl_store),
                #[cfg(feature = "test-support")]
                fault_injection: Some(fault),
                state,
            })
        } else {
            let session_id = uuid_v7();
            let timestamp = now_iso_timestamp();
            let cwd = cwd_override.unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| ".".to_owned())
            });
            let header = SessionHeader {
                version: Some(CURRENT_SESSION_VERSION),
                id: session_id.clone(),
                timestamp,
                cwd: cwd.clone(),
                parent_session: None,
            };
            Ok(Self {
                cwd,
                session_dir: dir,
                session_file: Some(resolved_path),
                store: Box::new(jsonl_store),
                #[cfg(feature = "test-support")]
                fault_injection: Some(fault),
                state: SessionMemoryState {
                    session_id,
                    file_entries: vec![FileEntry::Session(header)],
                    by_id: BTreeMap::new(),
                    labels_by_id: BTreeMap::new(),
                    label_timestamps_by_id: BTreeMap::new(),
                    leaf_id: None,
                    flushed: false,
                },
            })
        }
    }

    /// 创建纯内存会话（不落盘，支持从已有条目列表初始化）。
    pub fn in_memory(
        cwd: impl Into<String>,
        options: Option<NewSessionOptions>,
        preloaded_entries: Option<Vec<FileEntry>>,
    ) -> Result<Self, SessionStoreError> {
        let cwd = cwd.into();
        let opts = options.unwrap_or_default();
        if let Some(id) = &opts.id {
            assert_valid_session_id(id)?;
        }

        let mut file_entries = Vec::new();
        let session_id;
        if let Some(mut entries) = preloaded_entries
            && !entries.is_empty()
        {
            let _ = migrate_to_current_version(&mut entries);
            if let Some(FileEntry::Session(h)) = entries.first() {
                session_id = h.id.clone();
                file_entries = entries;
            } else {
                session_id = opts.id.unwrap_or_else(uuid_v7);
                let header = SessionHeader {
                    version: Some(CURRENT_SESSION_VERSION),
                    id: session_id.clone(),
                    timestamp: now_iso_timestamp(),
                    cwd: cwd.clone(),
                    parent_session: opts.parent_session,
                };
                file_entries.push(FileEntry::Session(header));
                file_entries.extend(entries);
            }
        } else {
            session_id = opts.id.unwrap_or_else(uuid_v7);
            let header = SessionHeader {
                version: Some(CURRENT_SESSION_VERSION),
                id: session_id.clone(),
                timestamp: now_iso_timestamp(),
                cwd: cwd.clone(),
                parent_session: opts.parent_session,
            };
            file_entries.push(FileEntry::Session(header));
        }

        let mut state = SessionMemoryState {
            session_id,
            file_entries: file_entries.clone(),
            by_id: BTreeMap::new(),
            labels_by_id: BTreeMap::new(),
            label_timestamps_by_id: BTreeMap::new(),
            leaf_id: None,
            flushed: false,
        };
        state.rebuild_index();

        Ok(Self {
            cwd,
            session_dir: PathBuf::new(),
            session_file: None,
            store: Box::new(InMemorySessionStore::from_entries(file_entries)),
            #[cfg(feature = "test-support")]
            fault_injection: None,
            state,
        })
    }

    /// 继续目录下最近一次会话，若不存在则新建。
    pub fn continue_recent(
        cwd: impl Into<String>,
        session_dir: impl Into<PathBuf>,
    ) -> Result<Self, SessionStoreError> {
        let cwd = cwd.into();
        let dir = session_dir.into();
        if let Some(recent) = find_most_recent_session(&dir, None) {
            Self::open(recent, Some(dir), Some(cwd))
        } else {
            Self::create(cwd, dir, None)
        }
    }

    /// 跨目录完整克隆源会话文件并创建新会话（对照 `SessionManager.forkFrom`）。
    pub fn fork_from(
        source_path: impl AsRef<Path>,
        target_cwd: impl Into<String>,
        session_dir: impl Into<PathBuf>,
        options: Option<NewSessionOptions>,
    ) -> Result<Self, SessionStoreError> {
        let source_path = source_path.as_ref();
        let target_cwd = target_cwd.into();
        let dir = session_dir.into();

        if !source_path.exists() {
            return Err(SessionStoreError::InvalidSessionFile(format!(
                "Cannot fork: source session file does not exist: {}",
                source_path.display()
            )));
        }

        let raw_content = fs::read_to_string(source_path).map_err(|e| {
            SessionStoreError::Io(format!(
                "Failed to read source session {}: {e}",
                source_path.display()
            ))
        })?;
        let mut source_entries = parse_session_entries(&raw_content);
        if source_entries.is_empty() {
            return Err(SessionStoreError::InvalidSessionFile(format!(
                "Cannot fork: source session file is empty or invalid: {}",
                source_path.display()
            )));
        }
        let has_header = matches!(source_entries.first(), Some(FileEntry::Session(_)));
        if !has_header {
            return Err(SessionStoreError::InvalidSessionFile(format!(
                "Cannot fork: source session has no header: {}",
                source_path.display()
            )));
        }
        let _ = migrate_to_current_version(&mut source_entries);

        let opts = options.unwrap_or_default();
        if let Some(id) = &opts.id {
            assert_valid_session_id(id)?;
        }
        let new_session_id = opts.id.unwrap_or_else(uuid_v7);
        let timestamp = now_iso_timestamp();
        let file_ts = timestamp.replace([':', '.'], "-");
        if !dir.as_os_str().is_empty() && !dir.exists() {
            fs::create_dir_all(&dir)
                .map_err(|e| SessionStoreError::Io(format!("Failed to create dir: {e}")))?;
        }
        let new_session_file = dir.join(format!("{file_ts}_{new_session_id}.jsonl"));

        let new_header = SessionHeader {
            version: Some(CURRENT_SESSION_VERSION),
            id: new_session_id,
            timestamp,
            cwd: target_cwd.clone(),
            parent_session: Some(source_path.to_string_lossy().into_owned()),
        };

        let mut new_entries = Vec::with_capacity(source_entries.len());
        new_entries.push(FileEntry::Session(new_header));
        for entry in source_entries {
            if matches!(entry, FileEntry::Entry(_)) {
                new_entries.push(entry);
            }
        }

        let mut store = JsonlSessionStore::open(&new_session_file)?;
        store.rewrite_all(&new_entries, true)?;
        drop(store);

        Self::open(new_session_file, Some(dir), Some(target_cwd))
    }

    /// 克隆一份纯内存的只读预览实例（用于 `turn_end` / `agent_before_settle` 边界草案校验）。
    pub fn clone_in_memory_preview(&self) -> Self {
        Self {
            cwd: self.cwd.clone(),
            session_dir: self.session_dir.clone(),
            session_file: self.session_file.clone(),
            store: Box::new(InMemorySessionStore::from_entries(
                self.state.file_entries.clone(),
            )),
            #[cfg(feature = "test-support")]
            fault_injection: None,
            state: self.state.clone(),
        }
    }

    #[cfg(feature = "test-support")]
    pub fn fault_injection(&self) -> Option<Arc<Mutex<JsonlFaultInjection>>> {
        self.fault_injection.clone()
    }

    pub fn last_tail_repair(&self) -> Option<&TailRepairOutcome> {
        self.store.last_tail_repair()
    }

    pub fn is_persisted(&self) -> bool {
        self.store.is_persisted()
    }

    pub fn is_poisoned(&self) -> bool {
        self.store.is_poisoned()
    }

    pub fn is_flushed(&self) -> bool {
        self.state.flushed
    }

    pub fn get_cwd(&self) -> &str {
        &self.cwd
    }

    pub fn get_session_dir(&self) -> &Path {
        &self.session_dir
    }

    pub fn get_session_id(&self) -> &str {
        &self.state.session_id
    }

    pub fn get_session_file(&self) -> Option<&Path> {
        self.session_file.as_deref()
    }

    pub fn get_header(&self) -> Option<&SessionHeader> {
        self.state.file_entries.iter().find_map(|e| match e {
            FileEntry::Session(h) => Some(h),
            _ => None,
        })
    }

    pub fn get_entry_count(&self) -> usize {
        self.state.by_id.len()
    }

    pub fn get_entries(&self) -> Vec<SessionEntry> {
        self.state.entries()
    }

    pub fn get_file_entries(&self) -> &[FileEntry] {
        &self.state.file_entries
    }

    pub fn get_leaf_id(&self) -> Option<&str> {
        self.state.leaf_id.as_deref()
    }

    pub fn get_leaf_entry(&self) -> Option<&SessionEntry> {
        self.state
            .leaf_id
            .as_deref()
            .and_then(|id| self.state.by_id.get(id))
    }

    pub fn get_entry(&self, id: &str) -> Option<&SessionEntry> {
        self.state.by_id.get(id)
    }

    pub fn get_children(&self, parent_id: &str) -> Vec<SessionEntry> {
        self.state
            .by_id
            .values()
            .filter(|e| e.parent_id() == Some(parent_id))
            .cloned()
            .collect()
    }

    pub fn get_label(&self, id: &str) -> Option<&str> {
        self.state.labels_by_id.get(id).map(String::as_str)
    }

    pub fn get_session_name(&self) -> Option<String> {
        for entry in self.state.file_entries.iter().rev() {
            if let FileEntry::Entry(SessionEntry::SessionInfo(info)) = entry {
                return info
                    .name
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned);
            }
        }
        None
    }

    pub fn get_branch(&self, from_id: Option<&str>) -> Vec<SessionEntry> {
        self.state.get_branch(from_id)
    }

    pub fn build_context_entries(&self) -> Vec<SessionEntry> {
        let entries = self.get_entries();
        build_context_entries(
            &entries,
            Some(self.state.leaf_id.as_deref()),
            Some(&self.state.by_id),
        )
    }

    pub fn build_session_projection(&self) -> SessionProjection {
        let entries = self.get_entries();
        build_session_projection(
            &entries,
            Some(self.state.leaf_id.as_deref()),
            Some(&self.state.by_id),
        )
    }

    pub fn build_session_context(&self) -> SessionContext {
        let entries = self.get_entries();
        build_session_context(
            &entries,
            Some(self.state.leaf_id.as_deref()),
            Some(&self.state.by_id),
        )
    }

    pub fn get_tree(&self) -> Vec<SessionTreeNode> {
        let entries = self.get_entries();
        let mut node_map: BTreeMap<String, SessionTreeNode> = BTreeMap::new();
        for entry in &entries {
            let id = entry.id().to_owned();
            node_map.insert(
                id.clone(),
                SessionTreeNode {
                    entry: entry.clone(),
                    children: Vec::new(),
                    label: self.state.labels_by_id.get(&id).cloned(),
                    label_timestamp: self.state.label_timestamps_by_id.get(&id).cloned(),
                },
            );
        }

        let mut children_ids_by_parent: BTreeMap<Option<String>, Vec<String>> = BTreeMap::new();
        for entry in &entries {
            let id = entry.id().to_owned();
            let parent_key = match entry.parent_id() {
                None => None,
                Some(pid) if pid == id || !node_map.contains_key(pid) => None,
                Some(pid) => Some(pid.to_owned()),
            };
            children_ids_by_parent
                .entry(parent_key)
                .or_default()
                .push(id);
        }

        let root_ids = children_ids_by_parent
            .get(&None)
            .cloned()
            .unwrap_or_default();
        let mut completed: BTreeMap<String, SessionTreeNode> = BTreeMap::new();
        let mut visited = BTreeSet::new();
        let mut roots = Vec::with_capacity(root_ids.len());

        for root_id in root_ids {
            let mut stack = vec![(root_id.clone(), false)];
            while let Some((id, exiting)) = stack.pop() {
                if exiting {
                    let Some(mut node) = node_map.remove(&id) else {
                        continue;
                    };
                    if let Some(child_ids) = children_ids_by_parent.get(&Some(id.clone())) {
                        let mut children: Vec<SessionTreeNode> = child_ids
                            .iter()
                            .filter_map(|child_id| completed.remove(child_id))
                            .collect();
                        children.sort_by_key(|child| {
                            parse_iso_timestamp_ms(child.entry.timestamp()).unwrap_or(0)
                        });
                        node.children = children;
                    }
                    completed.insert(id, node);
                    continue;
                }

                if !visited.insert(id.clone()) {
                    continue;
                }
                stack.push((id.clone(), true));
                if let Some(child_ids) = children_ids_by_parent.get(&Some(id)) {
                    for child_id in child_ids.iter().rev() {
                        stack.push((child_id.clone(), false));
                    }
                }
            }
            if let Some(root) = completed.remove(&root_id) {
                roots.push(root);
            }
        }

        roots.sort_by_key(|root| parse_iso_timestamp_ms(root.entry.timestamp()).unwrap_or(0));
        roots
    }

    // =========================================================================
    // Append & Persistence Methods (Atomic Commit After Sync)
    // =========================================================================

    fn commit_single_entry(&mut self, entry: SessionEntry) -> Result<String, SessionStoreError> {
        if self.store.is_poisoned() {
            return Err(SessionStoreError::Poisoned(
                "Session store is poisoned".to_owned(),
            ));
        }

        let entry_id = entry.id().to_owned();
        if self.store.is_persisted() {
            if !self.state.flushed {
                let triggers_conversation = self.state.has_conversation()
                    || matches!(
                        &entry,
                        SessionEntry::Message(m)
                            if m.message.role() == "user" || m.message.role() == "assistant"
                    );
                if triggers_conversation {
                    let mut candidate_entries =
                        Vec::with_capacity(self.state.file_entries.len() + 1);
                    candidate_entries.extend_from_slice(&self.state.file_entries);
                    candidate_entries.push(FileEntry::Entry(entry.clone()));
                    // 先完成排他创建与同步，成功后再更新内存树与 flushed 标记
                    self.store.rewrite_all(&candidate_entries, true)?;
                    self.state.flushed = true;
                }
            } else {
                // 先向磁盘追加并同步，成功后再发布到内存树
                self.store.append_entry(&entry)?;
            }
        } else {
            self.store.append_entry(&entry)?;
        }

        self.state.push_entry_in_memory(entry);
        Ok(entry_id)
    }

    pub fn append_message(&mut self, message: AgentMessage) -> Result<String, SessionStoreError> {
        if matches!(message.role(), "compactionSummary" | "branchSummary") {
            return Err(SessionStoreError::InvalidOperation(format!(
                "Cannot append {} via append_message; use append_compaction or branch_with_summary",
                message.role()
            )));
        }
        let entry = SessionEntry::Message(SessionMessageEntry {
            id: self.state.next_id(),
            parent_id: self.state.leaf_id.clone(),
            timestamp: now_iso_timestamp(),
            message,
        });
        self.commit_single_entry(entry)
    }

    /// Commit the input and its client correlation in one durable snapshot so a
    /// crash cannot leave a delivered user message without its deduplication key.
    pub fn append_correlated_user_message(
        &mut self,
        message: AgentMessage,
        client_message_id: &str,
    ) -> Result<String, SessionStoreError> {
        self.append_queued_user_message(message, client_message_id, None)
    }

    /// Atomically persist an input together with its delivery queue identity.
    pub fn append_queued_user_message(
        &mut self,
        message: AgentMessage,
        client_message_id: &str,
        queue_item_id: Option<&str>,
    ) -> Result<String, SessionStoreError> {
        if message.role() != "user" || self.store.is_poisoned() {
            return Err(SessionStoreError::InvalidOperation(
                "Correlated input requires a user message and a writable store".to_owned(),
            ));
        }
        let mut candidate = self.state.clone();
        let id = candidate.next_id();
        candidate.push_entry_in_memory(SessionEntry::Message(SessionMessageEntry {
            id: id.clone(),
            parent_id: candidate.leaf_id.clone(),
            timestamp: now_iso_timestamp(),
            message,
        }));
        candidate.push_entry_in_memory(SessionEntry::Custom(CustomEntry {
            id: candidate.next_id(),
            parent_id: candidate.leaf_id.clone(),
            timestamp: now_iso_timestamp(),
            custom_type: crate::execution_log::INPUT_CORRELATION_CUSTOM_TYPE.to_owned(),
            data: Some(serde_json::json!({
                "clientMessageId": client_message_id,
                "userEntryId": id,
                "queueItemId": queue_item_id,
            })),
        }));
        self.store.rewrite_all(
            &candidate.file_entries,
            self.store.is_persisted() && !candidate.flushed,
        )?;
        candidate.flushed = self.store.is_persisted();
        self.state = candidate;
        Ok(id)
    }

    pub fn append_thinking_level_change(
        &mut self,
        thinking_level: impl Into<String>,
    ) -> Result<String, SessionStoreError> {
        let entry = SessionEntry::ThinkingLevelChange(ThinkingLevelChangeEntry {
            id: self.state.next_id(),
            parent_id: self.state.leaf_id.clone(),
            timestamp: now_iso_timestamp(),
            thinking_level: thinking_level.into(),
        });
        self.commit_single_entry(entry)
    }

    pub fn append_model_change(
        &mut self,
        provider: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Result<String, SessionStoreError> {
        let entry = SessionEntry::ModelChange(ModelChangeEntry {
            id: self.state.next_id(),
            parent_id: self.state.leaf_id.clone(),
            timestamp: now_iso_timestamp(),
            provider: provider.into(),
            model_id: model_id.into(),
        });
        self.commit_single_entry(entry)
    }

    pub fn append_usage(
        &mut self,
        kind: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
        usage: Usage,
        note: Option<String>,
    ) -> Result<UsageEntry, SessionStoreError> {
        let usage_entry = UsageEntry {
            id: self.state.next_id(),
            parent_id: self.state.leaf_id.clone(),
            timestamp: now_iso_timestamp(),
            kind: kind.into(),
            provider: provider.into(),
            model: model.into(),
            usage,
            note,
        };
        self.commit_single_entry(SessionEntry::Usage(usage_entry.clone()))?;
        Ok(usage_entry)
    }

    pub fn append_compaction(
        &mut self,
        summary: impl Into<String>,
        first_kept_entry_id: Option<String>,
        tokens_before: u64,
        details: Option<JsonValue>,
        from_hook: Option<bool>,
        usage: Option<Usage>,
    ) -> Result<String, SessionStoreError> {
        let entry = self.state.create_compaction_entry(
            summary.into(),
            first_kept_entry_id,
            tokens_before,
            details,
            from_hook,
            usage,
        )?;
        self.commit_single_entry(entry)
    }

    pub fn append_custom_entry(
        &mut self,
        custom_type: impl Into<String>,
        data: Option<JsonValue>,
    ) -> Result<String, SessionStoreError> {
        let entry = SessionEntry::Custom(CustomEntry {
            id: self.state.next_id(),
            parent_id: self.state.leaf_id.clone(),
            timestamp: now_iso_timestamp(),
            custom_type: custom_type.into(),
            data,
        });
        self.commit_single_entry(entry)
    }

    pub fn append_session_info(
        &mut self,
        name: impl Into<String>,
    ) -> Result<String, SessionStoreError> {
        let raw = name.into();
        let sanitized = raw
            .chars()
            .map(|c| if matches!(c, '\r' | '\n') { ' ' } else { c })
            .collect::<String>()
            .trim()
            .to_owned();
        let entry = SessionEntry::SessionInfo(SessionInfoEntry {
            id: self.state.next_id(),
            parent_id: self.state.leaf_id.clone(),
            timestamp: now_iso_timestamp(),
            name: Some(sanitized),
        });
        self.commit_single_entry(entry)
    }

    pub fn append_custom_message_entry(
        &mut self,
        custom_type: impl Into<String>,
        content: impl Into<CustomMessageContent>,
        display: bool,
        details: Option<JsonValue>,
    ) -> Result<String, SessionStoreError> {
        let entry = SessionEntry::CustomMessage(CustomMessageEntry {
            id: self.state.next_id(),
            parent_id: self.state.leaf_id.clone(),
            timestamp: now_iso_timestamp(),
            custom_type: custom_type.into(),
            content: content.into(),
            details,
            display,
        });
        self.commit_single_entry(entry)
    }

    pub fn append_context_edit(
        &mut self,
        target_id: &str,
        replacement: Option<ContextEditReplacement>,
    ) -> Result<String, SessionStoreError> {
        let entry = self
            .state
            .validate_and_create_context_edit(target_id, replacement)?;
        self.commit_single_entry(entry)
    }

    pub fn append_label_change(
        &mut self,
        target_id: &str,
        label: Option<String>,
    ) -> Result<String, SessionStoreError> {
        if !self.state.by_id.contains_key(target_id) {
            return Err(SessionStoreError::InvalidOperation(format!(
                "Entry {target_id} not found"
            )));
        }
        let normalized = label.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
        let entry = SessionEntry::Label(LabelEntry {
            id: self.state.next_id(),
            parent_id: self.state.leaf_id.clone(),
            timestamp: now_iso_timestamp(),
            target_id: target_id.to_owned(),
            label: normalized,
        });
        self.commit_single_entry(entry)
    }

    /// 原子批量提交边界草案（`turn_end` / `agent_before_settle`）：
    /// 先在候选内存状态上完成全部验证、ID 分配与投影计算，再通过 `rewrite_all` 一次性原子提交完整快照。
    pub fn commit_boundary_drafts(
        &mut self,
        drafts: &[SessionBoundaryDraft],
    ) -> Result<Vec<SessionEntry>, SessionStoreError> {
        if self.store.is_poisoned() {
            return Err(SessionStoreError::Poisoned(
                "Session store is poisoned".to_owned(),
            ));
        }
        if drafts.is_empty() {
            return Ok(Vec::new());
        }

        let mut candidate = self.state.clone();
        let mut committed = Vec::with_capacity(drafts.len());

        for draft in drafts {
            let entry = match draft {
                SessionBoundaryDraft::Custom { custom_type, data } => {
                    SessionEntry::Custom(CustomEntry {
                        id: candidate.next_id(),
                        parent_id: candidate.leaf_id.clone(),
                        timestamp: now_iso_timestamp(),
                        custom_type: custom_type.clone(),
                        data: data.clone(),
                    })
                }
                SessionBoundaryDraft::CustomMessage {
                    custom_type,
                    content,
                    display,
                    details,
                } => SessionEntry::CustomMessage(CustomMessageEntry {
                    id: candidate.next_id(),
                    parent_id: candidate.leaf_id.clone(),
                    timestamp: now_iso_timestamp(),
                    custom_type: custom_type.clone(),
                    content: content.clone(),
                    details: details.clone(),
                    display: display.unwrap_or(true),
                }),
                SessionBoundaryDraft::ContextEdit {
                    target_id,
                    replacement,
                } => candidate.validate_and_create_context_edit(target_id, replacement.clone())?,
                SessionBoundaryDraft::Compaction {
                    summary,
                    first_kept_entry_id,
                    tokens_before,
                    details,
                    usage,
                    from_hook,
                } => candidate.create_compaction_entry(
                    summary.clone(),
                    first_kept_entry_id.clone(),
                    tokens_before.unwrap_or(0),
                    details.clone(),
                    *from_hook,
                    usage.clone(),
                )?,
            };
            candidate.push_entry_in_memory(entry.clone());
            committed.push(entry);
        }

        if self.store.is_persisted() {
            if candidate.flushed {
                self.store.rewrite_all(&candidate.file_entries, false)?;
            } else if candidate.has_conversation() {
                self.store.rewrite_all(&candidate.file_entries, true)?;
                candidate.flushed = true;
            }
        } else {
            self.store.rewrite_all(&candidate.file_entries, false)?;
        }

        self.state = candidate;
        Ok(committed)
    }

    // =========================================================================
    // Branching & Tree Navigation Operations
    // =========================================================================

    pub fn branch(&mut self, branch_from_id: &str) -> Result<(), SessionStoreError> {
        if !self.state.by_id.contains_key(branch_from_id) {
            return Err(SessionStoreError::InvalidOperation(format!(
                "Entry {branch_from_id} not found"
            )));
        }
        self.state.leaf_id = Some(branch_from_id.to_owned());
        Ok(())
    }

    pub fn reset_leaf(&mut self) {
        self.state.leaf_id = None;
    }

    pub fn branch_with_summary(
        &mut self,
        branch_from_id: Option<&str>,
        summary: impl Into<String>,
        details: Option<JsonValue>,
        from_hook: Option<bool>,
        usage: Option<Usage>,
    ) -> Result<String, SessionStoreError> {
        if let Some(id) = branch_from_id
            && !self.state.by_id.contains_key(id)
        {
            return Err(SessionStoreError::InvalidOperation(format!(
                "Entry {id} not found"
            )));
        }
        let prev_leaf = self.state.leaf_id.clone();
        let from_id = prev_leaf.clone().unwrap_or_else(|| "root".to_owned());
        self.state.leaf_id = branch_from_id.map(ToOwned::to_owned);

        let entry = SessionEntry::BranchSummary(BranchSummaryEntry {
            id: self.state.next_id(),
            parent_id: branch_from_id.map(ToOwned::to_owned),
            timestamp: now_iso_timestamp(),
            from_id,
            summary: summary.into(),
            details,
            usage,
            from_hook,
        });

        match self.commit_single_entry(entry) {
            Ok(id) => Ok(id),
            Err(err) => {
                self.state.leaf_id = prev_leaf;
                Err(err)
            }
        }
    }

    /// 提取从根到 `leaf_id` 的单条分支路径并派生新会话（剥离中间 `LabelEntry` 并重连父子链与 `first_kept_entry_id`）。
    pub fn create_branched_session(
        &mut self,
        leaf_id: &str,
    ) -> Result<Option<PathBuf>, SessionStoreError> {
        let path = self.get_branch(Some(leaf_id));
        if path.is_empty() {
            return Err(SessionStoreError::InvalidOperation(format!(
                "Entry {leaf_id} not found"
            )));
        }

        let previous_session_file = self
            .session_file
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned());

        let mut path_without_labels: Vec<SessionEntry> = Vec::new();
        let mut replacement_by_label_id: BTreeMap<String, String> = BTreeMap::new();
        let mut pending_label_ids: Vec<String> = Vec::new();
        let mut path_parent_id: Option<String> = None;

        for mut entry in path {
            if matches!(entry, SessionEntry::Label(_)) {
                pending_label_ids.push(entry.id().to_owned());
                continue;
            }
            for label_id in pending_label_ids.drain(..) {
                replacement_by_label_id.insert(label_id, entry.id().to_owned());
            }
            entry.set_parent_id(path_parent_id.clone());
            if let SessionEntry::Compaction(comp) = &mut entry
                && comp.first_kept_entry_id != comp.id
                && let Some(mapped) = replacement_by_label_id.get(&comp.first_kept_entry_id)
            {
                comp.first_kept_entry_id = mapped.clone();
            }
            path_parent_id = Some(entry.id().to_owned());
            path_without_labels.push(entry);
        }

        let new_session_id = uuid_v7();
        let timestamp = now_iso_timestamp();
        let file_ts = timestamp.replace([':', '.'], "-");
        let new_session_file = self
            .session_dir
            .join(format!("{file_ts}_{new_session_id}.jsonl"));

        let header = SessionHeader {
            version: Some(CURRENT_SESSION_VERSION),
            id: new_session_id.clone(),
            timestamp,
            cwd: self.cwd.clone(),
            parent_session: if self.is_persisted() {
                previous_session_file
            } else {
                None
            },
        };

        let mut path_entry_ids: BTreeSet<String> = path_without_labels
            .iter()
            .map(|e| e.id().to_owned())
            .collect();
        let mut labels_to_write: Vec<(String, String, String)> = Vec::new();
        for (target_id, label) in &self.state.labels_by_id {
            if path_entry_ids.contains(target_id) {
                let ts = self
                    .state
                    .label_timestamps_by_id
                    .get(target_id)
                    .cloned()
                    .unwrap_or_else(now_iso_timestamp);
                labels_to_write.push((target_id.clone(), label.clone(), ts));
            }
        }

        let mut parent_id = path_without_labels.last().map(|e| e.id().to_owned());
        let mut label_entries: Vec<SessionEntry> = Vec::new();
        for (target_id, label, label_ts) in labels_to_write {
            let id = generate_short_id(|candidate| path_entry_ids.contains(candidate));
            path_entry_ids.insert(id.clone());
            let le = SessionEntry::Label(LabelEntry {
                id: id.clone(),
                parent_id,
                timestamp: label_ts,
                target_id,
                label: Some(label),
            });
            parent_id = Some(id);
            label_entries.push(le);
        }

        let mut file_entries =
            Vec::with_capacity(1 + path_without_labels.len() + label_entries.len());
        file_entries.push(FileEntry::Session(header));
        for e in path_without_labels {
            file_entries.push(FileEntry::Entry(e));
        }
        for le in label_entries {
            file_entries.push(FileEntry::Entry(le));
        }

        let mut next_state = SessionMemoryState {
            session_id: new_session_id,
            file_entries,
            by_id: BTreeMap::new(),
            labels_by_id: BTreeMap::new(),
            label_timestamps_by_id: BTreeMap::new(),
            leaf_id: None,
            flushed: false,
        };
        next_state.rebuild_index();

        if self.is_persisted() {
            let mut new_store = JsonlSessionStore::open(&new_session_file)?;
            #[cfg(feature = "test-support")]
            let fault = new_store.fault_injection();
            if next_state.has_conversation() {
                new_store.rewrite_all(&next_state.file_entries, true)?;
                next_state.flushed = true;
            }
            self.store = Box::new(new_store);
            #[cfg(feature = "test-support")]
            {
                self.fault_injection = Some(fault);
            }
            self.session_file = Some(new_session_file.clone());
            self.state = next_state;
            Ok(Some(new_session_file))
        } else {
            self.store.rewrite_all(&next_state.file_entries, false)?;
            self.state = next_state;
            Ok(None)
        }
    }

    /// 主动取出并释放底层持久化 `SessionStore`（立即释放 `.lock` 文件锁），替换为只读墓碑实例。
    pub fn release_persistent_store(&mut self) {
        let loc = self.store.location().map(Path::to_path_buf);
        let was_persisted = self.store.is_persisted();
        let was_poisoned = self.store.is_poisoned();
        #[cfg(feature = "test-support")]
        {
            self.fault_injection = None;
        }
        self.store = Box::new(ClosedSessionStore::new(loc, was_persisted, was_poisoned));
    }

    /// 基于当前已结算的内存快照（无需重开原会话文件）从 `leaf_id` 派生一个全新的 `SessionManager` 实例。
    pub fn fork_branch_to_new_manager(&self, leaf_id: &str) -> Result<Self, SessionStoreError> {
        let mut preview = self.clone_in_memory_preview();
        preview.cwd = self.cwd.clone();
        preview.session_dir = self.session_dir.clone();
        preview.session_file = self.session_file.clone();
        if self.is_persisted() {
            // 让 create_branched_session 按持久化模式在新文件上创建 JsonlSessionStore
            preview.store = Box::new(ClosedSessionStore::new(
                self.session_file.clone(),
                true,
                false,
            ));
        }
        preview.create_branched_session(leaf_id)?;
        Ok(preview)
    }
}

// =============================================================================
// 6. Session Discovery, Info Extraction & Concurrent Listing
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub path: PathBuf,
    pub id: String,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_path: Option<String>,
    pub created_ms: u64,
    pub modified_ms: u64,
    pub message_count: usize,
    pub first_message: String,
    pub all_messages_text: String,
}

fn extract_message_text(msg: &AgentMessage) -> Option<String> {
    match msg {
        AgentMessage::User(u) => match &u.content {
            UserContent::Text(s) => Some(s.clone()),
            UserContent::Blocks(blocks) => {
                let text = blocks
                    .iter()
                    .filter_map(|b| match b {
                        UserContentBlock::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                Some(text)
            }
        },
        AgentMessage::Assistant(a) => {
            let text = a
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            Some(text)
        }
        _ => None,
    }
}

pub fn build_session_info_sync(file_path: &Path) -> Option<SessionInfo> {
    let metadata = fs::metadata(file_path).ok()?;
    let mtime_ms = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let header = read_session_header(file_path).ok().flatten()?;
    let raw = fs::read_to_string(file_path).ok()?;

    let mut message_count = 0usize;
    let mut first_message = String::new();
    let mut all_messages = Vec::new();
    let mut name: Option<String> = None;
    let mut last_activity_ms: Option<u64> = None;

    for line in raw.lines() {
        let Some(entry) = parse_session_entry_line(line) else {
            continue;
        };
        match entry {
            FileEntry::Session(_) => {}
            FileEntry::Entry(SessionEntry::SessionInfo(info)) => {
                name = info
                    .name
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned);
            }
            FileEntry::Entry(SessionEntry::Message(m)) => {
                message_count += 1;
                if matches!(m.message.role(), "user" | "assistant") {
                    let ts = if m.message.timestamp() > 0 {
                        m.message.timestamp()
                    } else {
                        parse_iso_timestamp_ms(&m.timestamp).unwrap_or(0)
                    };
                    if ts > 0 {
                        last_activity_ms = Some(last_activity_ms.unwrap_or(0).max(ts));
                    }
                    if let Some(text) = extract_message_text(&m.message)
                        && !text.is_empty()
                    {
                        if first_message.is_empty() && m.message.role() == "user" {
                            first_message = text.clone();
                        }
                        all_messages.push(text);
                    }
                }
            }
            _ => {}
        }
    }

    let created_ms = parse_iso_timestamp_ms(&header.timestamp).unwrap_or(mtime_ms);
    let modified_ms = last_activity_ms.unwrap_or(created_ms);

    Some(SessionInfo {
        path: file_path.to_path_buf(),
        id: header.id,
        cwd: header.cwd,
        name,
        parent_session_path: header.parent_session,
        created_ms,
        modified_ms,
        message_count,
        first_message: if first_message.is_empty() {
            "(no messages)".to_owned()
        } else {
            first_message
        },
        all_messages_text: all_messages.join(" "),
    })
}

/// 在目录中按修改时间倒序查找最近一个合法会话文件（使用 1 MiB 有界头部扫描）。
pub fn find_most_recent_session(session_dir: &Path, cwd_filter: Option<&str>) -> Option<PathBuf> {
    let entries = fs::read_dir(session_dir).ok()?;
    let mut files: Vec<(PathBuf, u128)> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jsonl"))
        .filter_map(|p| {
            let mtime = fs::metadata(&p)
                .ok()?
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_millis();
            Some((p, mtime))
        })
        .collect();

    files.sort_by_key(|a| std::cmp::Reverse(a.1));

    for (path, _) in files {
        if let Ok(Some(header)) = read_session_header(&path) {
            if let Some(expected_cwd) = cwd_filter
                && header.cwd != expected_cwd
            {
                continue;
            }
            return Some(path);
        }
    }
    None
}

/// 按精确会话 ID 在目录中查找会话文件（仅扫描头部，不加载正文）。
pub fn find_session_by_id(
    session_dir: &Path,
    id: &str,
    cwd_filter: Option<&str>,
) -> Option<PathBuf> {
    let entries = fs::read_dir(session_dir).ok()?;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        if let Ok(Some(header)) = read_session_header(&path)
            && header.id == id
        {
            if let Some(expected_cwd) = cwd_filter
                && header.cwd != expected_cwd
            {
                continue;
            }
            return Some(path);
        }
    }
    None
}

/// 并发加载目录下的会话摘要列表（受 `MAX_CONCURRENT_SESSION_INFO_LOADS = 10` 限制，结果按 `modified_ms` 降序排列）。
pub async fn list_sessions_from_dir(
    session_dir: &Path,
    cwd_filter: Option<&str>,
    cancel_token: Option<CancellationToken>,
) -> Vec<SessionInfo> {
    if cancel_token.as_ref().is_some_and(|t| t.is_cancelled()) || !session_dir.exists() {
        return Vec::new();
    }
    let Ok(read_dir) = fs::read_dir(session_dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = read_dir
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jsonl"))
        .collect();
    files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));

    let semaphore = Arc::new(tokio::sync::Semaphore::new(
        MAX_CONCURRENT_SESSION_INFO_LOADS,
    ));
    let mut handles = Vec::with_capacity(files.len());
    let cwd_owned = cwd_filter.map(ToOwned::to_owned);

    for path in files {
        let sem = semaphore.clone();
        let cancel = cancel_token.clone();
        let cwd_f = cwd_owned.clone();
        handles.push(tokio::spawn(async move {
            if cancel.as_ref().is_some_and(|t| t.is_cancelled()) {
                return None;
            }
            let _permit = sem.acquire_owned().await.ok()?;
            if cancel.as_ref().is_some_and(|t| t.is_cancelled()) {
                return None;
            }
            let info = tokio::task::spawn_blocking(move || build_session_info_sync(&path))
                .await
                .ok()??;
            if let Some(expected) = cwd_f
                && info.cwd != expected
            {
                return None;
            }
            Some(info)
        }));
    }

    let mut sessions = Vec::new();
    for handle in handles {
        if let Ok(Some(info)) = handle.await {
            sessions.push(info);
        }
    }
    sessions.sort_by_key(|a| std::cmp::Reverse(a.modified_ms));
    sessions
}

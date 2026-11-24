use std::collections::BTreeMap;
use std::sync::Arc;

use cst_os::{LaunchReceipt, UpdateReport};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::session_store::SessionViewSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ChannelModeDto {
    #[default]
    #[serde(rename = "app_active")]
    AppActive,
    #[serde(rename = "web_active")]
    WebActive,
}

impl ChannelModeDto {
    pub fn is_web_active(self) -> bool {
        matches!(self, Self::WebActive)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeModeDto {
    #[default]
    Light,
    Dark,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum KeyboardFocusStyleDto {
    #[default]
    Brackets,
    Rail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DefaultViewDto {
    #[default]
    Gui,
    Tui,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSettingsDto {
    #[serde(default)]
    pub theme: ThemeModeDto,
    #[serde(default)]
    pub keyboard_focus_style: KeyboardFocusStyleDto,
    #[serde(default)]
    pub default_view: DefaultViewDto,
    #[serde(default)]
    pub sidebar_collapsed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_x: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_y: Option<i32>,
    #[serde(default)]
    pub window_maximized: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<Value>,
}

impl Default for DesktopSettingsDto {
    fn default() -> Self {
        Self {
            theme: ThemeModeDto::Light,
            keyboard_focus_style: KeyboardFocusStyleDto::Brackets,
            default_view: DefaultViewDto::Gui,
            sidebar_collapsed: false,
            display_hint: None,
            window_width: None,
            window_height: None,
            window_x: None,
            window_y: None,
            window_maximized: false,
            terminal: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSettingsPatchDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<ThemeModeDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyboard_focus_style: Option<KeyboardFocusStyleDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_view: Option<DefaultViewDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidebar_collapsed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_hint: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_width: Option<Option<u32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_height: Option<Option<u32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_x: Option<Option<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_y: Option<Option<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_maximized: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<Option<Value>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub running: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelChoiceDto {
    pub provider: String,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub provider_name: Option<String>,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub credential_ready: bool,
    #[serde(default = "default_true")]
    pub catalog_present: bool,
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub thinking_levels: Vec<String>,
    #[serde(default)]
    pub thinking_level: Option<String>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelScopeMatchDto {
    pub id: String,
    pub pattern: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelScopeRuleDto {
    pub pattern: String,
    pub matches: Vec<ModelScopeMatchDto>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsSnapshot {
    #[serde(default)]
    pub models: Vec<ModelChoiceDto>,
    #[serde(default)]
    pub catalog: Vec<ModelChoiceDto>,
    #[serde(default)]
    pub enabled: Option<Vec<String>>,
    #[serde(default)]
    pub unavailable_enabled: Vec<String>,
    #[serde(default)]
    pub scope_rules: Option<Vec<ModelScopeRuleDto>>,
    #[serde(default)]
    pub selected: Option<ModelChoiceDto>,
    #[serde(default)]
    pub default_model: Option<ModelChoiceDto>,
    #[serde(default)]
    pub thinking_levels: Vec<String>,
    #[serde(default)]
    pub context_usage: Option<Value>,
    #[serde(default)]
    pub usage: Option<Value>,
}

impl ModelsSnapshot {
    pub fn any_model_configured(&self) -> bool {
        self.catalog.iter().any(|m| m.credential_ready)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStatusDto {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub supports_api_key: bool,
    #[serde(default)]
    pub supports_oauth: bool,
    #[serde(rename = "type", default)]
    pub auth_type: Option<String>,
    #[serde(default)]
    pub requires_login: bool,
    #[serde(default)]
    pub configured: Option<bool>,
    #[serde(default)]
    pub custom: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountProfileDto {
    #[serde(default)]
    pub supported: bool,
    #[serde(default)]
    pub student_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatusDto {
    pub provider_id: String,
    #[serde(default)]
    pub signed_in: bool,
    #[serde(default)]
    pub requires_login: bool,
    pub profile: AccountProfileDto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum UpdatePhaseDto {
    #[default]
    Unavailable,
    Idle,
    Checking,
    Current,
    Available,
    Preparing,
    Ready,
    Launching,
    ExitRequired,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatusDto {
    pub phase: UpdatePhaseDto,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub archive_bytes: Option<u64>,
    #[serde(default)]
    pub downloaded_bytes: Option<u64>,
    #[serde(default)]
    pub release_notes: Option<String>,
    #[serde(default)]
    pub operation_id: Option<String>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub launch: Option<LaunchReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStateDto {
    pub current_version: String,
    pub status: UpdateStatusDto,
    #[serde(default)]
    pub last_report: Option<UpdateReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppHealthDto {
    pub connected: bool,
    pub stopping: bool,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub process_created: Option<u64>,
    #[serde(default)]
    pub kit_root: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub elevated: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleSessionDto {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub pending_inbox_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleReportDto {
    #[serde(default)]
    pub stopping: bool,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub active_exports: usize,
    #[serde(default)]
    pub sessions: Vec<LifecycleSessionDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UiHintsDto {
    #[serde(default)]
    pub missing_key_prompted_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingKeyClaimDto {
    pub claimed: bool,
    #[serde(default)]
    pub missing_key_prompted_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebOpenDto {
    pub url: String,
    pub mode: ChannelModeDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentExportFileDto {
    pub name: String,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentExportSummary {
    pub id: String,
    pub session_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub captured_at: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub options: Value,
    #[serde(default)]
    pub files: Vec<RecentExportFileDto>,
}

impl RecentExportSummary {
    pub fn format(&self) -> &str {
        self.options
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("html")
    }

    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.bytes).sum()
    }

    pub fn page_count(&self) -> usize {
        self.files.len()
    }
}

/// Immutable, `Arc`-shared client state snapshot published by the background Tokio reducer.
#[derive(Debug, Clone)]
pub struct ClientSnapshot {
    pub revision: u64,
    pub connected: bool,
    pub reconnecting: bool,
    pub channel_mode: ChannelModeDto,
    pub version: String,
    pub commands: Arc<Vec<String>>,
    pub sessions: Arc<Vec<SessionSummary>>,
    pub loaded_sessions: Arc<BTreeMap<String, Arc<SessionViewSnapshot>>>,
    pub models: Option<Arc<ModelsSnapshot>>,
    pub auth_providers: Option<Arc<Vec<ProviderStatusDto>>>,
    pub account: Option<Arc<AccountStatusDto>>,
    pub desktop_settings: DesktopSettingsDto,
    pub desktop_settings_corrupt: bool,
    pub update_state: Option<Arc<UpdateStateDto>>,
    pub last_oauth_event: Option<Arc<Value>>,
}

impl Default for ClientSnapshot {
    fn default() -> Self {
        Self {
            revision: 0,
            connected: false,
            reconnecting: false,
            channel_mode: ChannelModeDto::AppActive,
            version: String::new(),
            commands: Arc::new(Vec::new()),
            sessions: Arc::new(Vec::new()),
            loaded_sessions: Arc::new(BTreeMap::new()),
            models: None,
            auth_providers: None,
            account: None,
            desktop_settings: DesktopSettingsDto::default(),
            desktop_settings_corrupt: false,
            update_state: None,
            last_oauth_event: None,
        }
    }
}

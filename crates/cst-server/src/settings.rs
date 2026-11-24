use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;

use crate::locks::{StorageError, atomic_write_json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    Light,
    Dark,
    System,
}

impl ThemeMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::System => "system",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DesktopViewMode {
    #[default]
    Gui,
    Tui,
}

impl DesktopViewMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gui => "gui",
            Self::Tui => "tui",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "gui" => Some(Self::Gui),
            "tui" => Some(Self::Tui),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyboardFocusStyle {
    #[default]
    Brackets,
    Rail,
}

impl KeyboardFocusStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Brackets => "brackets",
            Self::Rail => "rail",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "brackets" => Some(Self::Brackets),
            "rail" => Some(Self::Rail),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSettingsData {
    pub theme: ThemeMode,
    #[serde(default)]
    pub keyboard_focus_style: KeyboardFocusStyle,
    #[serde(default)]
    pub empty_model_scope: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSettingsData {
    #[serde(default)]
    pub theme: ThemeMode,
    #[serde(default)]
    pub keyboard_focus_style: KeyboardFocusStyle,
    #[serde(default)]
    pub default_view: DesktopViewMode,
    #[serde(default)]
    pub sidebar_collapsed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_x: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_y: Option<i32>,
    #[serde(default)]
    pub window_maximized: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<serde_json::Map<String, Value>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DesktopSettingsPatch {
    pub theme: Option<ThemeMode>,
    pub keyboard_focus_style: Option<KeyboardFocusStyle>,
    pub default_view: Option<DesktopViewMode>,
    pub sidebar_collapsed: Option<bool>,
    pub display_hint: Option<String>,
    pub window_width: Option<u32>,
    pub window_height: Option<u32>,
    pub window_x: Option<i32>,
    pub window_y: Option<i32>,
    pub window_maximized: Option<bool>,
    pub terminal: Option<serde_json::Map<String, Value>>,
}

#[derive(Debug, Clone)]
pub enum SettingsError {
    InvalidInput(String),
    ConfigurationUnavailable(String),
    Storage(StorageError),
}

impl SettingsError {
    pub fn status_code(&self) -> u16 {
        match self {
            Self::InvalidInput(_) => 400,
            Self::ConfigurationUnavailable(_) => 503,
            Self::Storage(s) => s.status_code(),
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "invalid_request",
            Self::ConfigurationUnavailable(_) => "configuration_unavailable",
            Self::Storage(s) => s.code(),
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::InvalidInput(m) | Self::ConfigurationUnavailable(m) => m.as_str(),
            Self::Storage(s) => &s.message,
        }
    }
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for SettingsError {}

/// Manages `data/config/web.json` (`theme` and `emptyModelScope`), isolated from TUI and Desktop themes.
#[derive(Clone)]
pub struct WebSettingsStore {
    file: PathBuf,
    serial: Arc<AsyncMutex<()>>,
}

impl WebSettingsStore {
    pub fn new(config_dir: impl AsRef<Path>) -> Self {
        Self {
            file: config_dir.as_ref().join("web.json"),
            serial: Arc::new(AsyncMutex::new(())),
        }
    }

    pub async fn read(&self) -> Result<WebSettingsData, SettingsError> {
        self.read_internal(None).await
    }

    async fn read_internal(
        &self,
        replacement_theme: Option<ThemeMode>,
    ) -> Result<WebSettingsData, SettingsError> {
        let path = self.file.clone();
        tokio::task::spawn_blocking(move || match fs::read(&path) {
            Ok(bytes) => {
                let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
                    SettingsError::ConfigurationUnavailable(
                        "Web外观配置无法读取，请检查 web.json 后重试。".to_owned(),
                    )
                })?;
                let Some(obj) = value.as_object() else {
                    return Err(SettingsError::ConfigurationUnavailable(
                        "Web外观配置无法读取，请检查 web.json 后重试。".to_owned(),
                    ));
                };
                let parsed_theme = obj
                    .get("theme")
                    .and_then(Value::as_str)
                    .and_then(ThemeMode::parse);
                let theme = match (parsed_theme, replacement_theme) {
                    (Some(t), _) => t,
                    (None, Some(rep)) => rep,
                    (None, None) => {
                        return Err(SettingsError::ConfigurationUnavailable(
                            "Web外观配置无法读取，请检查 web.json 后重试。".to_owned(),
                        ));
                    }
                };
                let keyboard_focus_style = obj
                    .get("keyboardFocusStyle")
                    .and_then(Value::as_str)
                    .and_then(KeyboardFocusStyle::parse)
                    .unwrap_or_default();
                let empty_model_scope = obj
                    .get("emptyModelScope")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Ok(WebSettingsData {
                    theme,
                    keyboard_focus_style,
                    empty_model_scope,
                })
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(WebSettingsData {
                theme: replacement_theme.unwrap_or(ThemeMode::Light),
                keyboard_focus_style: KeyboardFocusStyle::default(),
                empty_model_scope: false,
            }),
            Err(err) => Err(SettingsError::Storage(StorageError::from_io(
                &err,
                "读取 Web 设置",
            ))),
        })
        .await
        .map_err(|_| {
            SettingsError::ConfigurationUnavailable("读取 Web 设置任务异常结束".to_owned())
        })?
    }

    pub async fn save_theme(&self, theme: ThemeMode) -> Result<WebSettingsData, SettingsError> {
        self.save_preferences(Some(theme), None).await
    }

    pub async fn save_preferences(
        &self,
        theme: Option<ThemeMode>,
        keyboard_focus_style: Option<KeyboardFocusStyle>,
    ) -> Result<WebSettingsData, SettingsError> {
        let _guard = self.serial.lock().await;
        let mut current = self.read_internal(theme).await?;
        if let Some(t) = theme {
            current.theme = t;
        }
        if let Some(style) = keyboard_focus_style {
            current.keyboard_focus_style = style;
        }
        atomic_write_json(&self.file, &current, "保存 Web 设置")
            .await
            .map_err(SettingsError::Storage)?;
        Ok(current)
    }

    pub async fn save_empty_model_scope(
        &self,
        empty_model_scope: bool,
    ) -> Result<WebSettingsData, SettingsError> {
        let _guard = self.serial.lock().await;
        let mut current = self.read_internal(None).await?;
        current.empty_model_scope = empty_model_scope;
        atomic_write_json(&self.file, &current, "保存 Web 设置")
            .await
            .map_err(SettingsError::Storage)?;
        Ok(current)
    }
}

/// Manages `agent/home/desktop.json` (`theme`, `defaultView`, sidebar state, and window geometry),
/// isolated from Web (`web-settings.json`) and TUI (`settings.json`).
/// Preserves unknown top-level keys and unmodified unrecognized enum values on disk.
#[derive(Clone)]
pub struct DesktopSettingsStore {
    file: PathBuf,
    serial: Arc<AsyncMutex<()>>,
}

fn decode_desktop_map(map: &serde_json::Map<String, Value>) -> DesktopSettingsData {
    let theme = map
        .get("theme")
        .and_then(Value::as_str)
        .and_then(ThemeMode::parse)
        .unwrap_or_default();
    let keyboard_focus_style = map
        .get("keyboardFocusStyle")
        .and_then(Value::as_str)
        .and_then(KeyboardFocusStyle::parse)
        .unwrap_or_default();
    let default_view = map
        .get("defaultView")
        .and_then(Value::as_str)
        .and_then(DesktopViewMode::parse)
        .unwrap_or_default();
    let sidebar_collapsed = map
        .get("sidebarCollapsed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let display_hint = map
        .get("displayHint")
        .and_then(Value::as_str)
        .filter(|s| is_valid_display_hint(s))
        .map(str::to_owned);
    let window_width = map
        .get("windowWidth")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .filter(|w| (320..=16384).contains(w));
    let window_height = map
        .get("windowHeight")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .filter(|h| (240..=16384).contains(h));
    let window_x = map
        .get("windowX")
        .and_then(Value::as_i64)
        .and_then(|v| i32::try_from(v).ok())
        .filter(|x| (-32768..=32768).contains(x));
    let window_y = map
        .get("windowY")
        .and_then(Value::as_i64)
        .and_then(|v| i32::try_from(v).ok())
        .filter(|y| (-32768..=32768).contains(y));
    let window_maximized = map
        .get("windowMaximized")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let terminal = map.get("terminal").and_then(Value::as_object).cloned();

    DesktopSettingsData {
        theme,
        keyboard_focus_style,
        default_view,
        sidebar_collapsed,
        display_hint,
        window_width,
        window_height,
        window_x,
        window_y,
        window_maximized,
        terminal,
    }
}

fn is_valid_display_hint(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 64
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl DesktopSettingsStore {
    pub fn new(agent_dir: impl AsRef<Path>) -> Self {
        Self {
            file: agent_dir.as_ref().join("desktop.json"),
            serial: Arc::new(AsyncMutex::new(())),
        }
    }

    async fn read_raw_map(&self) -> Result<Option<serde_json::Map<String, Value>>, SettingsError> {
        let path = self.file.clone();
        tokio::task::spawn_blocking(move || match fs::read(&path) {
            Ok(bytes) => {
                let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
                    SettingsError::ConfigurationUnavailable(
                        "桌面端外观配置无法读取，请检查 desktop.json 后重试。".to_owned(),
                    )
                })?;
                let Some(obj) = value.as_object() else {
                    return Err(SettingsError::ConfigurationUnavailable(
                        "桌面端外观配置无法读取，请检查 desktop.json 后重试。".to_owned(),
                    ));
                };
                Ok(Some(obj.clone()))
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(SettingsError::Storage(StorageError::from_io(
                &err,
                "读取桌面端设置",
            ))),
        })
        .await
        .map_err(|_| {
            SettingsError::ConfigurationUnavailable("读取桌面端设置任务异常结束".to_owned())
        })?
    }

    pub async fn read(&self) -> Result<DesktopSettingsData, SettingsError> {
        match self.read_raw_map().await? {
            Some(map) => Ok(decode_desktop_map(&map)),
            None => Ok(DesktopSettingsData::default()),
        }
    }

    pub async fn patch(
        &self,
        patch: DesktopSettingsPatch,
    ) -> Result<DesktopSettingsData, SettingsError> {
        if patch
            .window_width
            .is_some_and(|w| !(320..=16384).contains(&w))
            || patch
                .window_height
                .is_some_and(|h| !(240..=16384).contains(&h))
            || patch
                .window_x
                .is_some_and(|x| !(-32768..=32768).contains(&x))
            || patch
                .window_y
                .is_some_and(|y| !(-32768..=32768).contains(&y))
        {
            return Err(SettingsError::InvalidInput("请求的窗口尺寸无效".to_owned()));
        }
        if patch
            .display_hint
            .as_deref()
            .is_some_and(|hint| !is_valid_display_hint(hint))
        {
            return Err(SettingsError::InvalidInput(
                "请求的显示器标识提示无效".to_owned(),
            ));
        }

        let _guard = self.serial.lock().await;
        let mut map = match self.read_raw_map().await? {
            Some(existing) => existing,
            None => {
                let mut init = serde_json::Map::new();
                init.insert(
                    "theme".to_owned(),
                    Value::String(ThemeMode::Light.as_str().to_owned()),
                );
                init.insert(
                    "defaultView".to_owned(),
                    Value::String(DesktopViewMode::Gui.as_str().to_owned()),
                );
                init.insert("sidebarCollapsed".to_owned(), Value::Bool(false));
                init.insert("windowMaximized".to_owned(), Value::Bool(false));
                init
            }
        };

        if let Some(theme) = patch.theme {
            map.insert("theme".to_owned(), Value::String(theme.as_str().to_owned()));
        } else if !map.contains_key("theme") {
            map.insert(
                "theme".to_owned(),
                Value::String(ThemeMode::Light.as_str().to_owned()),
            );
        }

        if let Some(style) = patch.keyboard_focus_style {
            map.insert(
                "keyboardFocusStyle".to_owned(),
                Value::String(style.as_str().to_owned()),
            );
        } else if !map.contains_key("keyboardFocusStyle") {
            map.insert(
                "keyboardFocusStyle".to_owned(),
                Value::String(KeyboardFocusStyle::default().as_str().to_owned()),
            );
        }

        if let Some(default_view) = patch.default_view {
            map.insert(
                "defaultView".to_owned(),
                Value::String(default_view.as_str().to_owned()),
            );
        } else if !map.contains_key("defaultView") {
            map.insert(
                "defaultView".to_owned(),
                Value::String(DesktopViewMode::Gui.as_str().to_owned()),
            );
        }

        if let Some(collapsed) = patch.sidebar_collapsed {
            map.insert("sidebarCollapsed".to_owned(), Value::Bool(collapsed));
        } else if !map.contains_key("sidebarCollapsed") {
            map.insert("sidebarCollapsed".to_owned(), Value::Bool(false));
        }

        if let Some(hint) = patch.display_hint {
            map.insert("displayHint".to_owned(), Value::String(hint));
        }
        if let Some(w) = patch.window_width {
            map.insert("windowWidth".to_owned(), Value::Number(w.into()));
        }
        if let Some(h) = patch.window_height {
            map.insert("windowHeight".to_owned(), Value::Number(h.into()));
        }
        if let Some(x) = patch.window_x {
            map.insert("windowX".to_owned(), Value::Number(x.into()));
        }
        if let Some(y) = patch.window_y {
            map.insert("windowY".to_owned(), Value::Number(y.into()));
        }
        if let Some(maximized) = patch.window_maximized {
            map.insert("windowMaximized".to_owned(), Value::Bool(maximized));
        } else if !map.contains_key("windowMaximized") {
            map.insert("windowMaximized".to_owned(), Value::Bool(false));
        }
        if let Some(terminal) = patch.terminal {
            map.insert("terminal".to_owned(), Value::Object(terminal));
        }

        atomic_write_json(&self.file, &Value::Object(map.clone()), "保存桌面端设置")
            .await
            .map_err(SettingsError::Storage)?;
        Ok(decode_desktop_map(&map))
    }
}

pub const MISSING_KEY_PROMPT_COOLDOWN_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiHintsData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_key_prompted_at_ms: Option<u64>,
    pub cooldown_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingKeyPromptClaimResult {
    pub claimed: bool,
    pub missing_key_prompted_at_ms: u64,
}

/// Manages `.state/ui-hints.json` shared across Web, Desktop, and TUI for runtime noise suppression
/// (such as the 24-hour missing-key initial prompt cooldown).
#[derive(Clone)]
pub struct UiHintsStore {
    file: PathBuf,
    serial: Arc<AsyncMutex<()>>,
}

impl UiHintsStore {
    pub fn new(kit_root: impl AsRef<Path>) -> Self {
        Self {
            file: kit_root.as_ref().join(".state").join("ui-hints.json"),
            serial: Arc::new(AsyncMutex::new(())),
        }
    }

    async fn read_map(&self) -> serde_json::Map<String, Value> {
        let path = self.file.clone();
        tokio::task::spawn_blocking(move || {
            fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .and_then(|val| val.as_object().cloned())
                .unwrap_or_default()
        })
        .await
        .unwrap_or_default()
    }

    pub async fn read(&self) -> UiHintsData {
        let map = self.read_map().await;
        let ts = map
            .get("missingKeyPromptedAtMs")
            .and_then(Value::as_u64)
            .filter(|&v| v > 0);
        UiHintsData {
            missing_key_prompted_at_ms: ts,
            cooldown_ms: MISSING_KEY_PROMPT_COOLDOWN_MS,
        }
    }

    pub async fn claim_missing_key_prompt(
        &self,
        client_last_prompted_at_ms: Option<u64>,
        now_ms: u64,
    ) -> Result<MissingKeyPromptClaimResult, SettingsError> {
        let _guard = self.serial.lock().await;
        let mut map = self.read_map().await;
        let disk_last = map
            .get("missingKeyPromptedAtMs")
            .and_then(Value::as_u64)
            .filter(|&v| v > 0 && v <= now_ms);
        let client_last = client_last_prompted_at_ms.filter(|&v| v > 0 && v <= now_ms);
        let merged_last = match (disk_last, client_last) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };

        if let Some(last) = merged_last
            && now_ms.saturating_sub(last) < MISSING_KEY_PROMPT_COOLDOWN_MS
        {
            if merged_last != disk_last {
                map.insert(
                    "missingKeyPromptedAtMs".to_owned(),
                    Value::Number(last.into()),
                );
                atomic_write_json(&self.file, &Value::Object(map), "保存运行态界面提示")
                    .await
                    .map_err(SettingsError::Storage)?;
            }
            return Ok(MissingKeyPromptClaimResult {
                claimed: false,
                missing_key_prompted_at_ms: last,
            });
        }

        map.insert(
            "missingKeyPromptedAtMs".to_owned(),
            Value::Number(now_ms.into()),
        );
        atomic_write_json(&self.file, &Value::Object(map), "保存运行态界面提示")
            .await
            .map_err(SettingsError::Storage)?;
        Ok(MissingKeyPromptClaimResult {
            claimed: true,
            missing_key_prompted_at_ms: now_ms,
        })
    }
}

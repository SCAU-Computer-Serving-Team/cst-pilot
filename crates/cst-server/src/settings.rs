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

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSettingsData {
    pub theme: ThemeMode,
    #[serde(default)]
    pub empty_model_scope: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSettingsData {
    #[serde(default)]
    pub theme: ThemeMode,
    #[serde(default)]
    pub default_view: DesktopViewMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_x: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_y: Option<i32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DesktopSettingsPatch {
    pub theme: Option<ThemeMode>,
    pub default_view: Option<DesktopViewMode>,
    pub window_width: Option<u32>,
    pub window_height: Option<u32>,
    pub window_x: Option<i32>,
    pub window_y: Option<i32>,
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

/// Manages `agent/home/web-settings.json` (`theme` and `emptyModelScope`), isolated from TUI and Desktop themes.
#[derive(Clone)]
pub struct WebSettingsStore {
    file: PathBuf,
    serial: Arc<AsyncMutex<()>>,
}

impl WebSettingsStore {
    pub fn new(agent_dir: impl AsRef<Path>) -> Self {
        Self {
            file: agent_dir.as_ref().join("web-settings.json"),
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
                        "Web外观配置无法读取，请检查 web-settings.json 后重试。".to_owned(),
                    )
                })?;
                let Some(obj) = value.as_object() else {
                    return Err(SettingsError::ConfigurationUnavailable(
                        "Web外观配置无法读取，请检查 web-settings.json 后重试。".to_owned(),
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
                            "Web外观配置无法读取，请检查 web-settings.json 后重试。".to_owned(),
                        ));
                    }
                };
                let empty_model_scope = obj
                    .get("emptyModelScope")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Ok(WebSettingsData {
                    theme,
                    empty_model_scope,
                })
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(WebSettingsData {
                theme: replacement_theme.unwrap_or(ThemeMode::Light),
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
        let _guard = self.serial.lock().await;
        let mut current = self.read_internal(Some(theme)).await?;
        current.theme = theme;
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

/// Manages `agent/home/desktop-settings.json` (`theme`, `defaultView`, and window geometry),
/// isolated from Web (`web-settings.json`) and TUI (`settings.json`).
#[derive(Clone)]
pub struct DesktopSettingsStore {
    file: PathBuf,
    serial: Arc<AsyncMutex<()>>,
}

impl DesktopSettingsStore {
    pub fn new(agent_dir: impl AsRef<Path>) -> Self {
        Self {
            file: agent_dir.as_ref().join("desktop-settings.json"),
            serial: Arc::new(AsyncMutex::new(())),
        }
    }

    pub async fn read(&self) -> Result<DesktopSettingsData, SettingsError> {
        let path = self.file.clone();
        tokio::task::spawn_blocking(move || match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<DesktopSettingsData>(&bytes).map_err(|_| {
                SettingsError::ConfigurationUnavailable(
                    "桌面端外观配置无法读取，请检查 desktop-settings.json 后重试。".to_owned(),
                )
            }),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(DesktopSettingsData::default()),
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
        {
            return Err(SettingsError::InvalidInput("请求的窗口尺寸无效".to_owned()));
        }
        let _guard = self.serial.lock().await;
        let mut current = self.read().await?;
        if let Some(theme) = patch.theme {
            current.theme = theme;
        }
        if let Some(default_view) = patch.default_view {
            current.default_view = default_view;
        }
        if let Some(w) = patch.window_width {
            current.window_width = Some(w);
        }
        if let Some(h) = patch.window_height {
            current.window_height = Some(h);
        }
        if let Some(x) = patch.window_x {
            current.window_x = Some(x);
        }
        if let Some(y) = patch.window_y {
            current.window_y = Some(y);
        }
        atomic_write_json(&self.file, &current, "保存桌面端设置")
            .await
            .map_err(SettingsError::Storage)?;
        Ok(current)
    }
}

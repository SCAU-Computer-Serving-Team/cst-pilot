use std::path::{Path, PathBuf};

/// Single source of truth for all bundled CST Pilot skills under `app/skills/<name>/SKILL.md`.
pub const REQUIRED_SKILL_NAMES: &[&str] = &[
    "disk",
    "ls",
    "files",
    "sys",
    "startup",
    "eventlog",
    "driver",
    "runbook",
    "bitlocker",
    "web-search",
];

/// Canonical four-zone path layout of a portable CST Pilot kit directory:
/// - `app/`: read-only program binaries, prompts, skills, fonts, telemetry assets, defaults, tools
/// - `data/` (overridable via `CST_PILOT_DATA_DIR`): persistent user data
/// - `artifacts/`: user-facing deliverables
/// - `.state/`: ephemeral runtime state, locks, update staging, and temp files
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KitLayout {
    root: PathBuf,
    data_dir: PathBuf,
}

impl KitLayout {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, String> {
        let canonical = root
            .as_ref()
            .canonicalize()
            .map_err(|_| "工具包目录无法读取。".to_owned())?;
        if !canonical.is_dir() {
            return Err("工具包根路径必须是目录。".to_owned());
        }
        let data_dir = resolve_data_dir(&canonical);
        Ok(Self {
            root: canonical,
            data_dir,
        })
    }

    pub fn with_data_dir(
        root: impl AsRef<Path>,
        data_dir: impl Into<PathBuf>,
    ) -> Result<Self, String> {
        let canonical = root
            .as_ref()
            .canonicalize()
            .map_err(|_| "工具包目录无法读取。".to_owned())?;
        if !canonical.is_dir() {
            return Err("工具包根路径必须是目录。".to_owned());
        }
        let raw_data = data_dir.into();
        let resolved_data = if raw_data.is_absolute() {
            raw_data
        } else {
            canonical.join(raw_data)
        };
        Ok(Self {
            root: canonical,
            data_dir: resolved_data,
        })
    }

    pub fn from_unchecked(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let data_dir = resolve_data_dir(&root);
        Self { root, data_dir }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn display_root(&self) -> PathBuf {
        strip_windows_verbatim_prefix(&self.root)
    }

    /// True when running directly inside the source repository (`Cargo.toml` at root).
    pub fn is_dev_workspace(&self) -> bool {
        self.root.join("Cargo.toml").is_file()
    }

    // ---- Program read-only zone (`app/`) ----

    pub fn app_dir(&self) -> PathBuf {
        self.root.join("app")
    }

    pub fn prompts_dir(&self) -> PathBuf {
        self.app_dir().join("prompts")
    }

    pub fn skills_dir(&self) -> PathBuf {
        self.app_dir().join("skills")
    }

    pub fn defaults_dir(&self) -> PathBuf {
        self.app_dir().join("defaults")
    }

    pub fn telemetry_assets_dir(&self) -> PathBuf {
        self.app_dir().join("telemetry")
    }

    pub fn licenses_dir(&self) -> PathBuf {
        self.app_dir().join("licenses")
    }

    pub fn tools_dir(&self) -> PathBuf {
        self.app_dir().join("tools")
    }

    /// Bundled fonts live under `app/fonts/`. Inside a source checkout without copied fonts,
    /// fall back to `<repo>/assets/fonts/`.
    pub fn fonts_dir(&self) -> PathBuf {
        let app_fonts = self.app_dir().join("fonts");
        if app_fonts.is_dir() || !self.is_dev_workspace() {
            app_fonts
        } else {
            let dev_fonts = self.root.join("assets").join("fonts");
            if dev_fonts.is_dir() {
                dev_fonts
            } else {
                app_fonts
            }
        }
    }

    /// Relative path (under `root`) to the active fonts directory for `Storage::pin_directory`.
    pub fn fonts_relative_dir(&self) -> &'static Path {
        if self.app_dir().join("fonts").is_dir() || !self.is_dev_workspace() {
            Path::new("app/fonts")
        } else if self.root.join("assets").join("fonts").is_dir() {
            Path::new("assets/fonts")
        } else {
            Path::new("app/fonts")
        }
    }

    pub fn lhm_dir(&self) -> PathBuf {
        let app_lhm = self.tools_dir().join("lhm");
        if app_lhm.is_dir() || !self.is_dev_workspace() {
            app_lhm
        } else {
            let dev_lhm = self.root.join("lhm");
            if dev_lhm.is_dir() { dev_lhm } else { app_lhm }
        }
    }

    pub fn desktop_exe(&self) -> PathBuf {
        self.root.join("cst-pilot.exe")
    }

    pub fn server_exe(&self) -> PathBuf {
        self.app_dir().join("cst-server.exe")
    }

    pub fn updater_exe(&self) -> PathBuf {
        self.app_dir().join("cst-updater.exe")
    }

    // ---- Persistent user data zone (`data/`, overridable via `CST_PILOT_DATA_DIR`) ----

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn display_data_dir(&self) -> PathBuf {
        strip_windows_verbatim_prefix(&self.data_dir)
    }

    pub fn config_dir(&self) -> PathBuf {
        self.data_dir.join("config")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.data_dir.join("sessions")
    }

    pub fn inbox_dir(&self) -> PathBuf {
        self.data_dir.join("inbox")
    }

    pub fn exports_dir(&self) -> PathBuf {
        self.data_dir.join("exports")
    }

    pub fn plans_dir(&self) -> PathBuf {
        self.data_dir.join("plans")
    }

    pub fn telemetry_data_dir(&self) -> PathBuf {
        self.data_dir.join("telemetry")
    }

    // ---- Ephemeral runtime zone (`.state/`) & deliverables (`artifacts/`) ----

    pub fn state_dir(&self) -> PathBuf {
        self.root.join(".state")
    }

    pub fn locks_dir(&self) -> PathBuf {
        self.state_dir().join("locks")
    }

    pub fn update_dir(&self) -> PathBuf {
        self.state_dir().join("update")
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.state_dir().join("tmp")
    }

    pub fn server_json_path(&self) -> PathBuf {
        self.state_dir().join("server.json")
    }

    pub fn ui_hints_path(&self) -> PathBuf {
        self.state_dir().join("ui-hints.json")
    }

    pub fn artifacts_dir(&self) -> PathBuf {
        self.root.join("artifacts")
    }

    pub fn normalized_key(&self) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(self.root.to_string_lossy().to_lowercase())
        } else {
            self.root.clone()
        }
    }
}

fn resolve_data_dir(root: &Path) -> PathBuf {
    if let Ok(explicit) = std::env::var("CST_PILOT_DATA_DIR") {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() {
            let p = PathBuf::from(trimmed);
            return if p.is_absolute() { p } else { root.join(p) };
        }
    }
    root.join("data")
}

pub fn strip_windows_verbatim_prefix(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{unc}"))
    } else if let Some(stripped) = raw.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        path.to_path_buf()
    }
}

pub fn normalize_canonical_path_for_hash(path: &Path) -> Result<String, String> {
    let canonical = path
        .canonicalize()
        .map_err(|_| "目录无法解析真实路径。".to_owned())?;
    let cleaned = strip_windows_verbatim_prefix(&canonical);
    let raw = cleaned.to_string_lossy();
    if cfg!(windows) {
        Ok(raw.to_lowercase())
    } else {
        Ok(raw.into_owned())
    }
}

fn looks_like_kit_root(dir: &Path) -> bool {
    dir.join("app")
        .join("prompts")
        .join("APPEND_SYSTEM.md")
        .is_file()
}

/// Resolve kit root strictly from `CST_PILOT_ROOT`, executable location (`<kit>` or `<kit>/app`),
/// or current working directory. Never reads `PI_CODING_AGENT_DIR` or `~/.pi/agent`.
pub fn resolve_kit_root() -> Result<PathBuf, String> {
    if let Ok(raw) = std::env::var("CST_PILOT_ROOT") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let candidate = PathBuf::from(trimmed);
            if candidate.is_dir() {
                return Ok(candidate);
            }
            return Err("CST_PILOT_ROOT 指向的目录不存在。".to_owned());
        }
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(exe_dir) = exe.parent()
    {
        if looks_like_kit_root(exe_dir) {
            return Ok(exe_dir.to_path_buf());
        }
        if exe_dir
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("app"))
            && let Some(parent) = exe_dir.parent()
            && looks_like_kit_root(parent)
        {
            return Ok(parent.to_path_buf());
        }
    }
    let current = std::env::current_dir().map_err(|_| "无法读取当前工作目录".to_owned())?;
    if looks_like_kit_root(&current) {
        return Ok(current);
    }
    Err("无法定位工具包根目录（缺少 app/prompts/APPEND_SYSTEM.md）。".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_layout_uses_data_and_app_zones() {
        let root = std::env::temp_dir().join(format!("cst-layout-rel-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("app/prompts")).unwrap();
        let layout = KitLayout::with_data_dir(&root, root.join("data")).unwrap();
        assert!(!layout.is_dev_workspace());
        assert_eq!(layout.app_dir(), layout.root().join("app"));
        assert_eq!(layout.config_dir(), layout.data_dir().join("config"));
        assert_eq!(layout.sessions_dir(), layout.data_dir().join("sessions"));
        assert_eq!(layout.inbox_dir(), layout.data_dir().join("inbox"));
        assert_eq!(layout.exports_dir(), layout.data_dir().join("exports"));
        assert_eq!(layout.plans_dir(), layout.data_dir().join("plans"));
        assert_eq!(
            layout.telemetry_data_dir(),
            layout.data_dir().join("telemetry")
        );
        assert_eq!(layout.locks_dir(), layout.root().join(".state/locks"));
        assert_eq!(
            layout.server_exe(),
            layout.root().join("app/cst-server.exe")
        );
        assert_eq!(
            layout.updater_exe(),
            layout.root().join("app/cst-updater.exe")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn dev_workspace_defaults_data_dir_to_data() {
        let root = std::env::temp_dir().join(format!("cst-layout-dev-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("assets/fonts")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        let layout = KitLayout::new(&root).unwrap();
        assert!(layout.is_dev_workspace());
        if std::env::var_os("CST_PILOT_DATA_DIR").is_none() {
            assert_eq!(layout.data_dir(), layout.root().join("data"));
            assert_eq!(layout.config_dir(), layout.root().join("data/config"));
        }
        assert_eq!(layout.fonts_dir(), layout.root().join("assets/fonts"));
        let _ = std::fs::remove_dir_all(root);
    }
}

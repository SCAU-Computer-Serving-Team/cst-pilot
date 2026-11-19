use std::path::{Path, PathBuf};

/// Canonical path layout of a portable CST Pilot kit directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KitLayout {
    root: PathBuf,
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
        Ok(Self { root: canonical })
    }

    pub fn from_unchecked(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn display_root(&self) -> PathBuf {
        strip_windows_verbatim_prefix(&self.root)
    }

    pub fn agent_dir(&self) -> PathBuf {
        self.root.join("agent").join("home")
    }

    pub fn state_dir(&self) -> PathBuf {
        self.root.join(".state")
    }

    pub fn update_dir(&self) -> PathBuf {
        self.state_dir().join("update")
    }

    pub fn server_json_path(&self) -> PathBuf {
        self.state_dir().join("server.json")
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

/// Resolve kit root from `PI_CODING_AGENT_DIR` (`<kit>/agent/home`) or current directory.
pub fn resolve_kit_root() -> Result<PathBuf, String> {
    if let Ok(agent_dir) = std::env::var("PI_CODING_AGENT_DIR")
        && !agent_dir.trim().is_empty()
    {
        let p = PathBuf::from(agent_dir.trim());
        if let Some(kit_root) = p.parent().and_then(|parent| parent.parent())
            && kit_root.join("agent").join("home").exists()
        {
            return Ok(kit_root.to_path_buf());
        }
    }
    let current = std::env::current_dir().map_err(|_| "无法读取当前工作目录".to_owned())?;
    if current.join("agent").join("home").exists() {
        return Ok(current);
    }
    Err("无法定位工具包根目录（缺少 agent/home）。".to_owned())
}

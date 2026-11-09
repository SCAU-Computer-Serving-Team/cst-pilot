use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use pi_runtime::ReadOperations;

const SECRET_FILES: &[&str] = &[
    "agent/home/auth.json",
    "agent/home/models.json",
    "agent/home/mcp-auth.json",
    "agent/home/web-search.json",
    "agent/home/telemetry/identity.json",
];

#[derive(Clone)]
pub struct PathGuard {
    root: PathBuf,
}

impl PathGuard {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, String> {
        let root = root
            .as_ref()
            .canonicalize()
            .map_err(|_| "工具包目录无法读取。")?;
        Ok(Self { root })
    }

    /// Resolve aliases before comparing; re-evaluate protected aliases on every access.
    pub fn check(&self, path: impl AsRef<Path>) -> Result<PathBuf, String> {
        let path = path.as_ref();
        reject_alternate_stream(path)?;
        let resolved = path.canonicalize().map_err(|_| "目标路径无法读取。")?;
        let key = path_key(&resolved);
        let state = self.root.join(".state");
        for candidate in [Some(state.clone()), state.canonicalize().ok()]
            .into_iter()
            .flatten()
        {
            let state_key = path_key(&candidate);
            if key == state_key || key.starts_with(&format!("{state_key}/")) {
                return Err(denied());
            }
        }
        if state.exists()
            && resolved
                .ancestors()
                .any(|ancestor| same_file::is_same_file(ancestor, &state).unwrap_or(false))
        {
            return Err(denied());
        }
        for relative in SECRET_FILES {
            let secret = self.root.join(relative);
            if key == path_key(&secret)
                || secret.canonicalize().is_ok_and(|p| path_key(&p) == key)
                || same_file::is_same_file(&resolved, &secret).unwrap_or(false)
            {
                return Err(denied());
            }
        }
        Ok(resolved)
    }

    pub fn open(&self, path: impl AsRef<Path>) -> Result<File, String> {
        let path = path.as_ref();
        let resolved = self.check(path)?;
        let file = File::open(&resolved).map_err(|_| "目标文件无法打开。")?;
        // Bind the checked name to the opened file and reject hard-link aliases.
        let handle = same_file::Handle::from_file(file.try_clone().map_err(|_| denied())?)
            .map_err(|_| denied())?;
        let current = self.check(path)?;
        if handle != same_file::Handle::from_path(current).map_err(|_| denied())? {
            return Err(denied());
        }
        for relative in SECRET_FILES {
            if let Ok(secret) = same_file::Handle::from_path(self.root.join(relative))
                && handle == secret
            {
                return Err(denied());
            }
        }
        if !file.metadata().map_err(|_| denied())?.is_file() {
            return Err("目标路径不是普通文件。".into());
        }
        Ok(file)
    }
}

fn denied() -> String {
    "权限拒绝：禁止读取或枚举凭据文件与运行态目录。".into()
}

fn path_key(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    let value = value.to_lowercase();
    value.trim_end_matches('/').to_owned()
}

fn reject_alternate_stream(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        // A colon is legal only in the drive prefix. Deny NT device paths too.
        let raw = path.to_string_lossy();
        let raw = raw.strip_prefix(r"\\?\").unwrap_or(&raw);
        if raw.starts_with(r"\\.\") || raw.char_indices().any(|(i, c)| c == ':' && i != 1) {
            return Err(denied());
        }
    }
    #[cfg(not(windows))]
    let _ = path;
    Ok(())
}

#[derive(Clone)]
pub struct GuardedReadOperations(pub PathGuard);

impl ReadOperations for GuardedReadOperations {
    fn access(&self, path: &Path) -> Result<(), String> {
        self.0.open(path).map(|_| ())
    }

    fn detect_image_mime_type(&self, path: &Path) -> Result<Option<String>, String> {
        let mut head = [0u8; 16];
        let n = self
            .0
            .open(path)?
            .read(&mut head)
            .map_err(|_| "文件类型无法读取。")?;
        let bytes = &head[..n];
        let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some("image/png")
        } else if bytes.starts_with(b"\xff\xd8\xff") {
            Some("image/jpeg")
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some("image/gif")
        } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            Some("image/webp")
        } else if bytes.starts_with(b"BM") {
            Some("image/bmp")
        } else {
            None
        };
        Ok(mime.map(str::to_owned))
    }

    fn read_bytes(&self, path: &Path) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        self.0
            .open(path)?
            .read_to_end(&mut bytes)
            .map_err(|_| "文件无法读取。")?;
        Ok(bytes)
    }

    fn open_reader(&self, path: &Path) -> Result<Box<dyn BufRead + Send>, String> {
        Ok(Box::new(BufReader::new(self.0.open(path)?)))
    }
}

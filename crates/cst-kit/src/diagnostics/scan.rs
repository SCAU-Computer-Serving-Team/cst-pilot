use super::mft::MftIndex;
use super::volume_index::{VolumeIdentity, display_path, in_tree, path_key};
use super::win_native::rounded;
use serde_json::{Value, json};
use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

pub const MIB: u64 = 1024 * 1024;
pub const STALE_MIN_BYTES: u64 = 50 * MIB;
pub const STALE_AGE_SECS: i64 = 365 * 86400;

pub fn ext_key(name: &str) -> String {
    name.rsplit_once('.')
        .filter(|(_, ext)| !ext.is_empty())
        .map(|(_, ext)| ext.to_lowercase().chars().take(12).collect::<String>())
        .unwrap_or_else(|| "(无扩展名)".into())
}

/// Shared deterministic comparator for hardlink representative selection across both `native-walk`
/// and `native-mft`: compares case-folded directory key, case-folded file name, and finally
/// breaks any case-only tie using the original directory and file names.
pub fn hardlink_rank_key<'a>(
    dir_key: &'a str,
    name: &'a str,
) -> (String, String, &'a str, &'a str) {
    (dir_key.to_uppercase(), name.to_uppercase(), dir_key, name)
}

/// Canonicalizes the longest existing ancestor of `path` and re-attaches any non-existent tail
/// components so pre-creation layout directories still match canonical scan paths.
fn canonicalize_best_effort(path: &Path) -> Option<PathBuf> {
    if let Ok(canon) = path.canonicalize() {
        return Some(canon);
    }
    let mut cur = path;
    let mut tail = Vec::new();
    while let Some(name) = cur.file_name() {
        tail.push(name.to_os_string());
        let Some(parent) = cur.parent() else {
            break;
        };
        if let Ok(mut canon) = parent.canonicalize() {
            for part in tail.into_iter().rev() {
                canon.push(part);
            }
            return Some(canon);
        }
        cur = parent;
    }
    None
}

#[cfg(not(windows))]
fn normalize_unix_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        if !matches!(comp, Component::CurDir) {
            out.push(comp.as_os_str());
        }
    }
    out
}

fn push_unique_mft_key(list: &mut Vec<String>, path: &Path) {
    let key = path_key(path);
    if !key.is_empty() && !list.contains(&key) {
        list.push(key);
    }
}

#[cfg(windows)]
fn push_unique_native_key(list: &mut Vec<String>, path: &Path) {
    push_unique_mft_key(list, path);
}

#[cfg(not(windows))]
fn push_unique_native_key(list: &mut Vec<PathBuf>, path: &Path) {
    let norm = normalize_unix_path(path);
    if !norm.as_os_str().is_empty() && !list.contains(&norm) {
        list.push(norm);
    }
}

fn is_config_secret_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("auth.json")
        || name.eq_ignore_ascii_case("models.json")
        || name.eq_ignore_ascii_case("mcp-auth.json")
        || name.eq_ignore_ascii_case("web-search.json")
}

fn is_identity_secret_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("identity.json")
}

fn segments_match_default_secret_dir(tail: [&str; 3], len: usize, is_config: bool) -> bool {
    let ends_with_2 = |a: &str, b: &str| {
        len >= 2 && tail[len - 2].eq_ignore_ascii_case(a) && tail[len - 1].eq_ignore_ascii_case(b)
    };
    let ends_with_3 = |a: &str, b: &str, c: &str| {
        len == 3
            && tail[0].eq_ignore_ascii_case(a)
            && tail[1].eq_ignore_ascii_case(b)
            && tail[2].eq_ignore_ascii_case(c)
    };
    if is_config {
        ends_with_2("data", "config") || ends_with_2("agent", "home")
    } else {
        ends_with_2("data", "telemetry") || ends_with_3("agent", "home", "telemetry")
    }
}

/// Unified sensitive-path filter shared across `walk`, `ls`, and `$MFT` backends.
/// Always protects default kit paths (`.state`, `data/config/*`, `data/telemetry/identity.json`,
/// and legacy `agent/home/*`) and additionally protects custom `KitLayout` directories (such as
/// `CST_PILOT_DATA_DIR` overrides).
#[derive(Clone, Debug, Default)]
pub struct SensitivePathFilter {
    #[cfg(windows)]
    extra_state_dirs: Vec<String>,
    #[cfg(windows)]
    extra_config_dirs: Vec<String>,
    #[cfg(windows)]
    extra_telemetry_dirs: Vec<String>,
    #[cfg(not(windows))]
    extra_state_dirs: Vec<PathBuf>,
    #[cfg(not(windows))]
    extra_config_dirs: Vec<PathBuf>,
    #[cfg(not(windows))]
    extra_telemetry_dirs: Vec<PathBuf>,
    // Windows-style uppercase `\` keys used for NTFS `$MFT` matching across all platforms/tests.
    extra_mft_state_dirs: Vec<String>,
    extra_mft_config_dirs: Vec<String>,
    extra_mft_telemetry_dirs: Vec<String>,
}

impl SensitivePathFilter {
    pub fn from_layout(layout: &cst_os::KitLayout) -> Self {
        Self::from_kit_dirs(
            Some(&layout.state_dir()),
            Some(&layout.config_dir()),
            Some(&layout.telemetry_data_dir()),
        )
    }

    pub fn from_kit_dirs(
        state_dir: Option<&Path>,
        config_dir: Option<&Path>,
        telemetry_dir: Option<&Path>,
    ) -> Self {
        let mut filter = Self::default();
        if let Some(dir) = state_dir {
            push_unique_native_key(&mut filter.extra_state_dirs, dir);
            push_unique_mft_key(&mut filter.extra_mft_state_dirs, dir);
            if let Some(canon) = canonicalize_best_effort(dir) {
                push_unique_native_key(&mut filter.extra_state_dirs, &canon);
                push_unique_mft_key(&mut filter.extra_mft_state_dirs, &canon);
            }
        }
        if let Some(dir) = config_dir {
            push_unique_native_key(&mut filter.extra_config_dirs, dir);
            push_unique_mft_key(&mut filter.extra_mft_config_dirs, dir);
            if let Some(canon) = canonicalize_best_effort(dir) {
                push_unique_native_key(&mut filter.extra_config_dirs, &canon);
                push_unique_mft_key(&mut filter.extra_mft_config_dirs, &canon);
            }
        }
        if let Some(dir) = telemetry_dir {
            push_unique_native_key(&mut filter.extra_telemetry_dirs, dir);
            push_unique_mft_key(&mut filter.extra_mft_telemetry_dirs, dir);
            if let Some(canon) = canonicalize_best_effort(dir) {
                push_unique_native_key(&mut filter.extra_telemetry_dirs, &canon);
                push_unique_mft_key(&mut filter.extra_mft_telemetry_dirs, &canon);
            }
        }
        filter
    }

    fn matches_extra_state_path(&self, dir: &Path) -> bool {
        if self.extra_state_dirs.is_empty() {
            return false;
        }
        #[cfg(windows)]
        {
            let key = path_key(dir);
            self.extra_state_dirs.iter().any(|s| in_tree(&key, s))
        }
        #[cfg(not(windows))]
        {
            let norm = normalize_unix_path(dir);
            self.extra_state_dirs.iter().any(|s| norm.starts_with(s))
        }
    }

    fn matches_extra_config_path(&self, dir: &Path) -> bool {
        if self.extra_config_dirs.is_empty() {
            return false;
        }
        #[cfg(windows)]
        {
            let key = path_key(dir);
            self.extra_config_dirs.contains(&key)
        }
        #[cfg(not(windows))]
        {
            let norm = normalize_unix_path(dir);
            self.extra_config_dirs.contains(&norm)
        }
    }

    fn matches_extra_telemetry_path(&self, dir: &Path) -> bool {
        if self.extra_telemetry_dirs.is_empty() {
            return false;
        }
        #[cfg(windows)]
        {
            let key = path_key(dir);
            self.extra_telemetry_dirs.contains(&key)
        }
        #[cfg(not(windows))]
        {
            let norm = normalize_unix_path(dir);
            self.extra_telemetry_dirs.contains(&norm)
        }
    }

    /// Checks a native filesystem directory `Path` using platform path components (so literal `\`
    /// characters in Unix filenames are never treated as separators).
    pub fn is_sensitive_dir_path(&self, dir: &Path) -> bool {
        for comp in dir.components() {
            if let Component::Normal(seg) = comp
                && seg.to_string_lossy().eq_ignore_ascii_case(".state")
            {
                return true;
            }
        }
        self.matches_extra_state_path(dir)
    }

    /// Checks a native filesystem `(dir, name)` entry using platform path components plus any
    /// custom `KitLayout` directories.
    pub fn is_sensitive_entry_path(&self, dir: &Path, name: &str) -> bool {
        if name.eq_ignore_ascii_case(".state") || self.is_sensitive_dir_path(dir) {
            return true;
        }
        if !self.extra_state_dirs.is_empty() && self.matches_extra_state_path(&dir.join(name)) {
            return true;
        }
        let is_config = is_config_secret_name(name);
        let is_identity = !is_config && is_identity_secret_name(name);
        if !is_config && !is_identity {
            return false;
        }
        if is_config && self.matches_extra_config_path(dir) {
            return true;
        }
        if is_identity && self.matches_extra_telemetry_path(dir) {
            return true;
        }
        let mut tail = [String::new(), String::new(), String::new()];
        let mut len = 0usize;
        for comp in dir.components() {
            if let Component::Normal(seg) = comp {
                let text = seg.to_string_lossy().into_owned();
                if len < 3 {
                    tail[len] = text;
                    len += 1;
                } else {
                    tail.swap(0, 1);
                    tail.swap(1, 2);
                    tail[2] = text;
                }
            }
        }
        segments_match_default_secret_dir(
            [tail[0].as_str(), tail[1].as_str(), tail[2].as_str()],
            len,
            is_config,
        )
    }

    /// Checks a full native filesystem path (file or directory) for kit sensitivity.
    pub fn is_sensitive_full_path(&self, path: &Path) -> bool {
        if self.is_sensitive_dir_path(path) {
            return true;
        }
        match (path.parent(), path.file_name().and_then(|n| n.to_str())) {
            (Some(parent), Some(name)) => self.is_sensitive_entry_path(parent, name),
            _ => false,
        }
    }

    fn mft_full_key(drive_upper: &str, rel_dir: &str) -> String {
        if rel_dir.is_empty() {
            drive_upper.to_ascii_uppercase()
        } else {
            path_key(Path::new(&format!("{drive_upper}\\{rel_dir}")))
        }
    }

    /// Checks an NTFS `$MFT` relative directory (`\`-separated) on `drive_upper`.
    pub fn is_sensitive_mft_dir(&self, drive_upper: &str, rel_dir: &str) -> bool {
        if rel_dir
            .split('\\')
            .filter(|seg| !seg.is_empty() && *seg != ".")
            .any(|seg| seg.eq_ignore_ascii_case(".state"))
        {
            return true;
        }
        if self.extra_mft_state_dirs.is_empty() {
            return false;
        }
        let full = Self::mft_full_key(drive_upper, rel_dir);
        self.extra_mft_state_dirs.iter().any(|s| in_tree(&full, s))
    }

    /// Checks an NTFS `$MFT` entry `(rel_dir, name)` (`\`-separated) on `drive_upper`.
    pub fn is_sensitive_mft_entry(&self, drive_upper: &str, rel_dir: &str, name: &str) -> bool {
        if name.eq_ignore_ascii_case(".state") || self.is_sensitive_mft_dir(drive_upper, rel_dir) {
            return true;
        }
        if !self.extra_mft_state_dirs.is_empty() {
            let child_rel = if rel_dir.is_empty() {
                name.to_owned()
            } else {
                format!("{rel_dir}\\{name}")
            };
            let full_child = Self::mft_full_key(drive_upper, &child_rel);
            if self
                .extra_mft_state_dirs
                .iter()
                .any(|s| in_tree(&full_child, s))
            {
                return true;
            }
        }
        let is_config = is_config_secret_name(name);
        let is_identity = !is_config && is_identity_secret_name(name);
        if !is_config && !is_identity {
            return false;
        }
        let mut tail = [""; 3];
        let mut len = 0usize;
        for seg in rel_dir
            .split('\\')
            .filter(|seg| !seg.is_empty() && *seg != ".")
        {
            if len < 3 {
                tail[len] = seg;
                len += 1;
            } else {
                tail[0] = tail[1];
                tail[1] = tail[2];
                tail[2] = seg;
            }
        }
        if segments_match_default_secret_dir(tail, len, is_config) {
            return true;
        }
        let full_dir = Self::mft_full_key(drive_upper, rel_dir);
        if is_config {
            self.extra_mft_config_dirs.contains(&full_dir)
        } else {
            self.extra_mft_telemetry_dirs.contains(&full_dir)
        }
    }
}

/// Returns `true` when any Windows/NTFS path component of `dir` is `.state` (case-insensitive ASCII).
pub fn is_sensitive_dir_str(dir: &str) -> bool {
    dir.split(['\\', '/'])
        .filter(|seg| !seg.is_empty() && *seg != ".")
        .any(|seg| seg.eq_ignore_ascii_case(".state"))
}

pub fn is_sensitive_dir_path(dir: &Path) -> bool {
    SensitivePathFilter::default().is_sensitive_dir_path(dir)
}

/// Returns `true` when string-formatted `(dir, name)` matches default protected kit paths.
pub fn is_sensitive_entry_str(dir: &str, name: &str) -> bool {
    if name.eq_ignore_ascii_case(".state") || is_sensitive_dir_str(dir) {
        return true;
    }
    let is_config = is_config_secret_name(name);
    let is_identity = !is_config && is_identity_secret_name(name);
    if !is_config && !is_identity {
        return false;
    }
    let mut tail = [""; 3];
    let mut len = 0usize;
    for seg in dir
        .split(['\\', '/'])
        .filter(|seg| !seg.is_empty() && *seg != ".")
    {
        if len < 3 {
            tail[len] = seg;
            len += 1;
        } else {
            tail[0] = tail[1];
            tail[1] = tail[2];
            tail[2] = seg;
        }
    }
    segments_match_default_secret_dir(tail, len, is_config)
}

pub fn is_sensitive_entry_path(dir: &Path, name: &str) -> bool {
    SensitivePathFilter::default().is_sensitive_entry_path(dir, name)
}

/// Checks a full file or directory path for default kit sensitivity.
pub fn is_sensitive_full_path(path: &Path) -> bool {
    SensitivePathFilter::default().is_sensitive_full_path(path)
}

pub fn format_modified_utc(unix_secs: i64) -> Option<String> {
    if unix_secs <= 0 {
        return None;
    }
    chrono::DateTime::from_timestamp(unix_secs, 0)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.2} GB", bytes as f64 / 1073741824.0)
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / 1048576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ScanMethod {
    #[default]
    NativeMft,
    NativeWalk,
}

impl ScanMethod {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NativeMft => "native-mft",
            Self::NativeWalk => "native-walk",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ranked {
    pub bytes: u64,
    pub path: String,
    pub modified: Option<String>,
}
impl Ord for Ranked {
    fn cmp(&self, other: &Self) -> Ordering {
        self.bytes
            .cmp(&other.bytes)
            .then_with(|| self.path.cmp(&other.path))
            .then_with(|| self.modified.cmp(&other.modified))
    }
}
impl PartialOrd for Ranked {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub struct TopN {
    limit: usize,
    rows: BinaryHeap<Reverse<Ranked>>,
}
impl TopN {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            rows: BinaryHeap::new(),
        }
    }
    pub fn limit(&self) -> usize {
        self.limit
    }
    pub fn would_accept(&self, bytes: u64) -> bool {
        if self.limit == 0 {
            return false;
        }
        self.rows.len() < self.limit
            || self
                .rows
                .peek()
                .is_some_and(|smallest| bytes >= smallest.0.bytes)
    }
    pub fn add(&mut self, row: Ranked) {
        if self.limit == 0 {
            return;
        }
        if self.rows.len() < self.limit {
            self.rows.push(Reverse(row));
        } else if self.rows.peek().is_some_and(|smallest| row > smallest.0) {
            self.rows.pop();
            self.rows.push(Reverse(row));
        }
    }
    pub fn sorted(self) -> Vec<Ranked> {
        self.rows
            .into_sorted_vec()
            .into_iter()
            .map(|r| r.0)
            .collect()
    }
}

#[derive(Clone)]
pub struct ScanSnapshot {
    pub root: String,
    pub identity: Option<VolumeIdentity>,
    pub captured_at: Instant,
    pub method: ScanMethod,
    pub mft_index: Option<Arc<MftIndex>>,
    pub dirs: HashMap<String, u64>,
    pub files: HashMap<String, u64>,
    pub total: u64,
    pub rows: usize,
    pub file_count: usize,
    pub top_dirs: Vec<Ranked>,
    pub top_files: Vec<Ranked>,
    pub stale: Vec<Ranked>,
    pub extensions: HashMap<String, (u64, u64)>,
    pub ext_overflow: bool,
}

impl Default for ScanSnapshot {
    fn default() -> Self {
        Self {
            root: String::new(),
            identity: None,
            captured_at: Instant::now(),
            method: ScanMethod::NativeMft,
            mft_index: None,
            dirs: HashMap::new(),
            files: HashMap::new(),
            total: 0,
            rows: 0,
            file_count: 0,
            top_dirs: Vec::new(),
            top_files: Vec::new(),
            stale: Vec::new(),
            extensions: HashMap::new(),
            ext_overflow: false,
        }
    }
}

impl ScanSnapshot {
    pub fn estimated_bytes(&self) -> usize {
        let index_bytes = self
            .mft_index
            .as_ref()
            .map_or(0, |idx| idx.estimated_bytes());
        let map_bytes = self
            .dirs
            .keys()
            .map(|k| k.len() + 48)
            .sum::<usize>()
            .saturating_add(self.files.keys().map(|k| k.len() + 48).sum::<usize>())
            .saturating_add(self.extensions.len().saturating_mul(64));
        index_bytes.saturating_add(map_bytes).saturating_add(4096)
    }

    pub fn covers_target(&self, target: &Path) -> bool {
        if let Some(mft) = &self.mft_index {
            mft.covers_path(target)
        } else {
            self.dirs.contains_key(&path_key(target))
        }
    }

    pub fn lookup_dir(&self, file_id: Option<u64>, path: &Path) -> Option<u64> {
        if let Some(mft) = &self.mft_index {
            mft.lookup_dir(file_id, path)
        } else {
            self.dirs.get(&path_key(path)).copied()
        }
    }

    pub fn lookup_file(&self, path: &Path) -> Option<u64> {
        self.files.get(&path_key(path)).copied()
    }

    pub fn merge(&self, incoming: &Self) -> Self {
        if incoming.mft_index.is_some() {
            return incoming.clone();
        }
        let mut merged = incoming.clone();
        let root = path_key(Path::new(&incoming.root));
        for (key, bytes) in &self.dirs {
            if !in_tree(key, &root) {
                merged.dirs.insert(key.clone(), *bytes);
            }
        }
        for (key, bytes) in &self.files {
            if !in_tree(key, &root) {
                merged.files.insert(key.clone(), *bytes);
            }
        }
        // A subtree refresh does not make any old ancestor aggregate current.
        merged
            .dirs
            .retain(|key, _| !in_tree(&root, key) || key == &root);
        merged
    }

    pub fn usage(&self, root: &Path, top: usize, filesystem: Option<&str>) -> Value {
        let top_dirs = self
            .top_dirs
            .iter()
            .take(top)
            .map(|r| json!({"path": r.path, "sizeGB": gib(r.bytes), "pct": pct(r.bytes, self.total)}))
            .collect::<Vec<_>>();
        let top_files = self
            .top_files
            .iter()
            .take(top)
            .map(|r| {
                json!({
                    "path": r.path,
                    "sizeGB": gib(r.bytes),
                    "modified": r.modified
                })
            })
            .collect::<Vec<_>>();
        let mut extensions: Vec<_> = self.extensions.iter().collect();
        extensions.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(b.0)));
        let ext_note = if self.ext_overflow {
            "扩展名种类超过 65,536 上限，extAgg 可能为下界。"
        } else {
            ""
        };
        json!({
            "method": self.method.as_str(),
            "root": display_path(root),
            "totalGB": gib(self.total),
            "topDirs": top_dirs,
            "topFiles": top_files,
            "extAgg": extensions
                .into_iter()
                .take(top.min(40))
                .map(|(ext, (bytes, files))| json!({"ext": ext, "files": files, "sizeGB": gib(*bytes)}))
                .collect::<Vec<_>>(),
            "staleFiles": self
                .stale
                .iter()
                .take(top)
                .map(|r| json!({"path": r.path, "sizeGB": gib(r.bytes), "modified": r.modified}))
                .collect::<Vec<_>>(),
            "stats": {
                "filesScanned": self.file_count,
            },
            "notice": format!(
                "纯 Rust 原生索引统计 {} 条记录，其中文件 {} 个。文件系统：{}。{ext_note}目录排行包含不同深度，父子目录不可相加；陈旧文件为至少 50 MiB 且一年未修改。",
                self.rows,
                self.file_count,
                filesystem.unwrap_or("未知")
            )
        })
    }
}

pub fn gib(bytes: u64) -> f64 {
    let value = bytes as f64 / 1073741824.0;
    if bytes == 0 {
        return 0.0;
    }
    [2, 4, 6, 12]
        .into_iter()
        .map(|digits| rounded(value, digits))
        .find(|v| *v > 0.0)
        .unwrap_or(value)
}

pub fn pct(bytes: u64, total: u64) -> Option<f64> {
    (total > 0).then(|| rounded(bytes as f64 / total as f64 * 100.0, 1))
}

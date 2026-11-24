use super::mft::MftIndex;
use super::volume_index::{VolumeIdentity, display_path, in_tree, path_key};
use super::win_native::rounded;
use serde_json::{Value, json};
use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap};
use std::path::Path;
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

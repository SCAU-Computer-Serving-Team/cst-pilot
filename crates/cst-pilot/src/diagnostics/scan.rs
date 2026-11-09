use super::win_native::{Control, rounded};
use super::wz_index::{in_tree, path_key};
use chrono::{Local, NaiveDateTime, TimeZone};
use serde_json::{Value, json};
use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

const MIB: u64 = 1024 * 1024;
const INDEX_BYTES: usize = 128 * 1024 * 1024;
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
    pub fn add(&mut self, row: Ranked) {
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
#[derive(Clone, Default)]
pub struct ScanSnapshot {
    pub root: String,
    pub identity: Option<super::wz_index::VolumeIdentity>,
    pub dirs: HashMap<String, u64>,
    pub files: HashMap<String, u64>,
    pub total: u64,
    pub rows: usize,
    pub file_count: usize,
    pub top_dirs: Vec<Ranked>,
    pub top_files: Vec<Ranked>,
    pub stale: Vec<Ranked>,
    pub extensions: HashMap<String, (u64, u64)>,
}
impl ScanSnapshot {
    pub fn merge(&self, incoming: &Self) -> Self {
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
        let rows = |items: &[Ranked]| {
            items
                .iter()
                .take(top)
                .map(|r| json!({"path":r.path,"sizeGB":gib(r.bytes),"pct":pct(r.bytes,self.total)}))
                .collect::<Vec<_>>()
        };
        let mut extensions: Vec<_> = self.extensions.iter().collect();
        extensions.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(b.0)));
        json!({
            "method":if filesystem.is_some_and(|fs| fs.eq_ignore_ascii_case("NTFS")) {"wiztree-mft"} else {"wiztree-walk"},
            "root":root.to_string_lossy(),"totalGB":gib(self.total),
            "topDirs":rows(&self.top_dirs),"topFiles":rows(&self.top_files),
            "extAgg":extensions.into_iter().take(top.min(40)).map(|(ext,(bytes,files))|json!({"ext":ext,"files":files,"sizeGB":gib(*bytes)})).collect::<Vec<_>>(),
            "staleFiles":self.stale.iter().take(top).map(|r|json!({"path":r.path,"sizeGB":gib(r.bytes),"modified":r.modified})).collect::<Vec<_>>(),
            "notice":format!("WizTree 完整导出 {} 行，其中文件 {} 个。文件系统：{}。目录排行包含不同深度，父子目录不可相加；陈旧文件为至少 50 MiB 且一年未修改。",self.rows,self.file_count,filesystem.unwrap_or("未知，按目录遍历标注"))
        })
    }
}
/// Each physical record is bounded before handing it to csv. Windows paths do
/// not contain newlines, so an unterminated quoted record is rejected, not buffered.
pub fn parse_export(
    reader: Box<dyn Read + Send>,
    root: &Path,
    control: &Control,
) -> Result<ScanSnapshot, String> {
    let mut reader = BufReader::new(reader);
    let mut result = ScanSnapshot {
        root: root.to_string_lossy().into_owned(),
        ..Default::default()
    };
    let root_key = path_key(root);
    let mut dirs = TopN::new(100);
    let mut files = TopN::new(100);
    let mut stale = TopN::new(100);
    let mut line = Vec::new();
    let mut index_bytes = 0usize;
    let now = Local::now().timestamp();
    loop {
        control.check()?;
        line.clear();
        let count = reader
            .by_ref()
            .take(65537)
            .read_until(b'\n', &mut line)
            .map_err(|_| "WizTree CSV 读取失败。")?;
        if count == 0 {
            break;
        }
        if count > 65536 {
            return Err("WizTree CSV 单行超过 64 KiB。".into());
        }
        let line = line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&line);
        let mut csv = csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .from_reader(line);
        let Some(row) = csv.records().next() else {
            continue;
        };
        let row = row.map_err(|_| "WizTree CSV 格式无效。")?;
        let Some(path) = row.get(0) else {
            continue;
        };
        let Some(bytes) = row.get(1).and_then(|s| s.parse::<u64>().ok()) else {
            if path.contains(":\\") || path.contains(":/") {
                return Err("WizTree CSV 数据行大小无效。".into());
            }
            continue;
        };
        let key = path_key(Path::new(path));
        if !in_tree(&key, &root_key) || path.split(['\\', '/']).any(|s| s == "." || s == "..") {
            return Err("WizTree CSV 包含扫描范围外的路径。".into());
        }
        result.rows += 1;
        let is_dir = path.ends_with(['\\', '/']);
        if is_dir {
            if result.dirs.insert(key.clone(), bytes).is_some() {
                return Err("WizTree CSV 包含重复目录。".into());
            }
            index_bytes = index_bytes.saturating_add(key.len() + 64);
            if key == root_key {
                result.total = bytes;
            } else {
                dirs.add(Ranked {
                    path: path.into(),
                    bytes,
                    modified: None,
                });
            }
        } else {
            result.file_count += 1;
            files.add(Ranked {
                path: path.into(),
                bytes,
                modified: None,
            });
            if bytes >= MIB {
                result.files.insert(key.clone(), bytes);
                index_bytes = index_bytes.saturating_add(key.len() + 64);
            }
            let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
            let ext = name
                .rsplit_once('.')
                .filter(|(_, ext)| !ext.is_empty())
                .map(|(_, ext)| ext.to_lowercase().chars().take(12).collect::<String>())
                .unwrap_or_else(|| "(无扩展名)".into());
            if !result.extensions.contains_key(&ext) && result.extensions.len() >= 65536 {
                return Err("WizTree 扩展名种类超过扫描预算。".into());
            }
            let agg = result.extensions.entry(ext).or_default();
            agg.0 = agg.0.checked_add(bytes).ok_or("WizTree 文件大小溢出。")?;
            agg.1 += 1;
            let modified = row.iter().skip(2).find_map(|s| {
                NaiveDateTime::parse_from_str(s.trim(), "%Y/%m/%d %H:%M:%S")
                    .ok()
                    .map(|date| (s, date))
            });
            if bytes >= 50 * MIB
                && let Some((text, date)) = modified
                && let Some(date) = Local.from_local_datetime(&date).earliest()
                && now - date.timestamp() >= 365 * 86400
            {
                stale.add(Ranked {
                    path: path.into(),
                    bytes,
                    modified: Some(text.into()),
                });
            }
        }
        if index_bytes > INDEX_BYTES {
            return Err("WizTree 卷索引超过 128 MiB 内存预算。".into());
        }
    }
    if !result.dirs.contains_key(&root_key) {
        return Err("WizTree CSV 缺少扫描根目录，未发布缓存。".into());
    }
    result.top_dirs = dirs.sorted();
    result.top_files = files.sorted();
    result.stale = stale.sorted();
    control.check()?;
    Ok(result)
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

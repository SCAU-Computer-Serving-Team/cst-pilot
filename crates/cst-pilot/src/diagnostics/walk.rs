use super::scan::{Ranked, TopN, gib, pct};
use super::win_native::Control;
use serde_json::{Value, json};
use std::fs::{self, Metadata, ReadDir};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub struct WalkLimits {
    pub entries: usize,
    pub timeout: Duration,
}
impl Default for WalkLimits {
    fn default() -> Self {
        Self {
            entries: 500_000,
            timeout: Duration::from_secs(30),
        }
    }
}
pub struct WalkBudget {
    pub scanned: usize,
    pub incomplete: bool,
    pub denied: Vec<String>,
    start: Instant,
    limits: WalkLimits,
}
impl WalkBudget {
    pub fn new(limits: WalkLimits) -> Self {
        Self {
            scanned: 0,
            incomplete: false,
            denied: Vec::new(),
            start: Instant::now(),
            limits,
        }
    }
    pub fn take(&mut self, control: &Control) -> Result<bool, String> {
        if control.cancel.is_cancelled() {
            control.check()?;
        }
        if self.scanned >= self.limits.entries
            || self.start.elapsed() >= self.limits.timeout
            || control.remaining().is_zero()
        {
            self.incomplete = true;
            return Ok(false);
        }
        self.scanned += 1;
        Ok(true)
    }
    pub fn failed(&mut self, path: &Path) {
        self.incomplete = true;
        if self.denied.len() < 50 {
            self.denied.push(path.to_string_lossy().into_owned());
        }
    }
}
pub fn linked(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}
struct Frame {
    path: PathBuf,
    entries: ReadDir,
    bytes: u64,
}
pub fn size(
    root: &Path,
    budget: &mut WalkBudget,
    control: &Control,
    top: &mut TopN,
) -> Result<u64, String> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => {
            budget.failed(root);
            return Ok(0);
        }
    };
    let mut stack = vec![Frame {
        path: root.into(),
        entries,
        bytes: 0,
    }];
    while !stack.is_empty() {
        if !budget.take(control)? {
            break;
        }
        let frame = stack.last_mut().expect("nonempty traversal stack");
        match frame.entries.next() {
            Some(Ok(entry)) => {
                let path = entry.path();
                let meta = match fs::symlink_metadata(&path) {
                    Ok(m) => m,
                    Err(_) => {
                        budget.failed(&path);
                        break;
                    }
                };
                if linked(&meta) {
                    budget.incomplete = true;
                    continue;
                }
                if meta.is_dir() {
                    if stack.len() >= 1024 {
                        budget.failed(&path);
                        break;
                    }
                    match fs::read_dir(&path) {
                        Ok(entries) => stack.push(Frame {
                            path,
                            entries,
                            bytes: 0,
                        }),
                        Err(_) => {
                            budget.failed(&path);
                            break;
                        }
                    }
                } else if meta.is_file() {
                    frame.bytes = frame.bytes.saturating_add(meta.len());
                }
            }
            Some(Err(_)) => {
                let path = frame.path.clone();
                budget.failed(&path);
                break;
            }
            None => {
                // Exhaustion checks are not directory entries.
                budget.scanned = budget.scanned.saturating_sub(1);
                let frame = stack.pop().expect("nonempty traversal stack");
                top.add(Ranked {
                    bytes: frame.bytes,
                    path: frame.path.to_string_lossy().into_owned(),
                    modified: None,
                });
                if let Some(parent) = stack.last_mut() {
                    parent.bytes = parent.bytes.saturating_add(frame.bytes);
                } else {
                    return Ok(frame.bytes);
                }
            }
        }
    }
    // Preserve all already measured bytes while propagating the partial sums.
    let mut bytes = 0u64;
    for frame in stack.into_iter().rev() {
        bytes = bytes.saturating_add(frame.bytes);
        top.add(Ranked {
            bytes,
            path: frame.path.to_string_lossy().into_owned(),
            modified: None,
        });
    }
    Ok(bytes)
}
pub fn usage(
    root: &Path,
    top: usize,
    control: &Control,
    limits: WalkLimits,
) -> Result<Value, String> {
    let start = Instant::now();
    let mut budget = WalkBudget::new(limits);
    let mut largest = TopN::new(top + 1);
    let total = size(root, &mut budget, control, &mut largest)?;
    let root_key = super::wz_index::path_key(root);
    let rows=largest.sorted().into_iter().filter(|r|super::wz_index::path_key(Path::new(&r.path))!=root_key).take(top).map(|r|json!({"path":r.path,"sizeGB":gib(r.bytes),"pct":if budget.incomplete {None}else{pct(r.bytes,total)}})).collect::<Vec<_>>();
    Ok(
        json!({"method":"node-walk","root":root.to_string_lossy(),"totalGB":gib(total),"topDirs":rows,"stats":{"filesScanned":budget.scanned,"elapsedSec":super::win_native::rounded(start.elapsed().as_secs_f64(),1),"budget":limits.entries},"truncated":budget.incomplete,"denied":budget.denied,"notice":if budget.incomplete {"扫描因预算、链接或访问限制未完成，大小为下界，占比未计算。"}else{"原生目录遍历完成，仅提供目录排行；不同深度的父子目录不可相加。"}}),
    )
}

use super::scan::TopN;
use super::walk::{WalkBudget, WalkLimits, linked};
use super::win_native::{Control, rounded};
use super::wz_index::{WzIndex, display_path, path_key};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LsInput {
    pub path: String,
    pub top: Option<usize>,
}
struct Item {
    name: String,
    kind: &'static str,
    bytes: Option<u64>,
}
pub fn run(
    index: &WzIndex,
    target: &Path,
    top: usize,
    control: &Control,
    limits: WalkLimits,
) -> Result<Value, String> {
    let meta = fs::symlink_metadata(target).map_err(|_| "目标目录无法读取。")?;
    if linked(&meta) || !meta.is_dir() {
        return Err("目标必须是目录且不能是链接或重解析点。".into());
    }
    let scan_control = Control::new(
        control.cancel.child_token(),
        std::time::Duration::from_secs(180).min(control.remaining()),
    );
    let snapshot = match index.scan(target, false, &scan_control) {
        Ok(snapshot) => snapshot,
        Err(_) => {
            control.check()?;
            None
        }
    };
    let entries = fs::read_dir(target).map_err(|_| "无法列出目标目录。")?;
    measure(
        index,
        target,
        top,
        control,
        limits,
        snapshot.as_deref(),
        entries,
    )
}

fn measure(
    index: &WzIndex,
    target: &Path,
    top: usize,
    control: &Control,
    limits: WalkLimits,
    snapshot: Option<&super::scan::ScanSnapshot>,
    entries: impl IntoIterator<Item = std::io::Result<fs::DirEntry>>,
) -> Result<Value, String> {
    let mut items = Vec::new();
    let mut used_cache = false;
    let mut walked = false;
    let mut budget = WalkBudget::new(limits);
    let mut enumeration = WalkBudget::new(limits);
    let mut children_truncated = false;
    for entry in entries {
        if !enumeration.take(control)? {
            budget.incomplete = true;
            children_truncated = true;
            break;
        }
        let Ok(entry) = entry else {
            budget.incomplete = true;
            continue;
        };
        let path = entry.path();
        let (cached_dir, cached_file) = snapshot.map_or((None, None), |s| {
            let key = path_key(&path);
            (s.dirs.get(&key).copied(), s.files.get(&key).copied())
        });
        let (kind, bytes) = match fs::symlink_metadata(&path) {
            Ok(meta) if linked(&meta) => {
                budget.incomplete = true;
                ("unknown", None)
            }
            Ok(meta) if meta.is_dir() => {
                let size = if let Some(bytes) = cached_dir {
                    used_cache = true;
                    Some(bytes)
                } else if !budget.exhausted && budget.take(control)? {
                    walked = true;
                    Some(super::walk::size(
                        &path,
                        &mut budget,
                        control,
                        &mut TopN::new(0),
                    )?)
                } else {
                    budget.incomplete = true;
                    None
                };
                ("dir", size)
            }
            Ok(meta) if meta.is_file() => ("file", Some(meta.len())),
            Err(_) => match (cached_dir, cached_file) {
                (Some(size), _) => {
                    used_cache = true;
                    ("dir", Some(size))
                }
                (_, Some(size)) => {
                    used_cache = true;
                    ("file", Some(size))
                }
                _ => {
                    budget.incomplete = true;
                    ("unknown", None)
                }
            },
            Ok(_) => {
                budget.incomplete = true;
                ("unknown", None)
            }
        };
        items.push(Item {
            name: entry.file_name().to_string_lossy().into_owned(),
            kind,
            bytes,
        });
    }
    if snapshot.is_some_and(|snapshot| !index.is_current(snapshot, target)) {
        return Err("目录读取期间卷身份变化，请重试。".into());
    }
    items.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    let total = items
        .iter()
        .filter_map(|i| i.bytes)
        .fold(0u64, u64::saturating_add);
    let mut result = json!({"path":display_path(target),"totalChildren":items.len(),"totalSize":format_bytes(total),"method":if used_cache && !walked {"wiztree-index"}else{"walk"},"entries":items.iter().take(top).map(|i|json!({"name":i.name,"type":i.kind,"bytes":i.bytes,"size":i.bytes.map(format_bytes),"pct":if budget.incomplete || total==0 {None}else{i.bytes.map(|v|rounded(v as f64/total as f64*100.0,1))}})).collect::<Vec<_>>()});
    if items.len() > top {
        let omitted = &items[top..];
        result["omitted"] = json!({"count":omitted.len(),"unknownCount":omitted.iter().filter(|i|i.bytes.is_none()).count(),"size":format_bytes(omitted.iter().filter_map(|i|i.bytes).fold(0u64,u64::saturating_add)),"note":"已按已知大小截断，未知大小不参与排序"});
    }
    if budget.incomplete {
        result["notice"] =
            json!("部分子项因预算、链接或访问限制未统计完整，大小为下界，全部占比未计算。");
    }
    if walked && used_cache {
        let notice = result["notice"].as_str().unwrap_or_default();
        result["notice"] = json!(format!(
            "部分子目录使用原生遍历，其余可用大小来自 WizTree 缓存或文件元数据。{notice}"
        ));
    }
    if children_truncated {
        result["childrenTruncated"] = json!(true);
    }
    Ok(result)
}
fn format_bytes(bytes: u64) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ls_keeps_readable_siblings_after_directory_entry_error() {
        let dir = std::env::temp_dir().join(format!("cst-ls-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("readable"), b"test").unwrap();
        let entries = std::iter::once(Err(std::io::Error::from(
            std::io::ErrorKind::PermissionDenied,
        )))
        .chain(fs::read_dir(&dir).unwrap());
        let index = WzIndex::new(std::sync::Arc::new(
            super::super::volume::NativeScanSource::new(&dir),
        ));
        let result = measure(
            &index,
            &dir,
            10,
            &Control::new(Default::default(), std::time::Duration::from_secs(3)),
            WalkLimits::default(),
            None,
            entries,
        );
        fs::remove_dir_all(&dir).unwrap();
        let result = result.unwrap();
        assert_eq!(result["totalChildren"], 1);
        assert_eq!(result["entries"][0]["bytes"], 4);
        assert!(result["entries"][0]["pct"].is_null());
        assert!(result["notice"].is_string());
    }
}

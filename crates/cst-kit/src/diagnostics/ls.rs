use super::scan::TopN;
use super::walk::{WalkBudget, WalkLimits, linked};
use super::win_native::{Control, rounded};
use super::wz_index::{WzIndex, path_key};
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
    let mut items = Vec::new();
    let mut budget = WalkBudget::new(limits);
    let mut children_truncated = false;
    for entry in entries {
        if control.cancel.is_cancelled() {
            control.check()?;
        }
        if items.len() >= limits.entries {
            budget.incomplete = true;
            children_truncated = true;
            break;
        }
        let entry = entry.map_err(|_| "无法读取目录子项。")?;
        if !budget.take(control)? {
            budget.incomplete = true;
            items.push(Item {
                name: entry.file_name().to_string_lossy().into_owned(),
                kind: "unknown",
                bytes: None,
            });
            children_truncated = true;
            break;
        }
        let path = entry.path();
        let key = path_key(&path);
        let cached_dir = snapshot.as_ref().and_then(|s| s.dirs.get(&key)).copied();
        let cached_file = snapshot.as_ref().and_then(|s| s.files.get(&key)).copied();
        let (kind, bytes) = match fs::symlink_metadata(&path) {
            Ok(meta) if linked(&meta) => {
                budget.incomplete = true;
                ("unknown", None)
            }
            Ok(meta) if meta.is_dir() => {
                let size = if let Some(bytes) = cached_dir {
                    Some(bytes)
                } else if !budget.incomplete {
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
            Ok(meta) if meta.is_file() => (
                "file",
                Some(if meta.len() > 0 {
                    meta.len()
                } else {
                    cached_file.unwrap_or(0)
                }),
            ),
            _ => match (cached_dir, cached_file) {
                (Some(size), _) => ("dir", Some(size)),
                (_, Some(size)) => ("file", Some(size)),
                _ => {
                    budget.incomplete = true;
                    ("unknown", None)
                }
            },
        };
        items.push(Item {
            name: entry.file_name().to_string_lossy().into_owned(),
            kind,
            bytes,
        });
    }
    if snapshot
        .as_ref()
        .is_some_and(|snapshot| !index.is_current(snapshot, target))
    {
        return Err("目录读取期间卷身份变化，请重试。".into());
    }
    items.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    let total = items
        .iter()
        .filter_map(|i| i.bytes)
        .fold(0u64, u64::saturating_add);
    let mut result = json!({"path":target.to_string_lossy(),"totalChildren":items.len(),"totalSize":format_bytes(total),"method":if snapshot.is_some(){"wiztree-index"}else{"walk"},"entries":items.iter().take(top).map(|i|json!({"name":i.name,"type":i.kind,"bytes":i.bytes,"size":i.bytes.map(format_bytes),"pct":if budget.incomplete || total==0 {None}else{i.bytes.map(|v|rounded(v as f64/total as f64*100.0,1))}})).collect::<Vec<_>>()});
    if items.len() > top {
        let omitted = &items[top..];
        result["omitted"] = json!({"count":omitted.len(),"unknownCount":omitted.iter().filter(|i|i.bytes.is_none()).count(),"size":format_bytes(omitted.iter().filter_map(|i|i.bytes).fold(0u64,u64::saturating_add)),"note":"已按已知大小截断，未知大小不参与排序"});
    }
    if budget.incomplete {
        result["notice"] =
            json!("部分子项因预算、链接或访问限制未统计完整，大小为下界，全部占比未计算。");
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

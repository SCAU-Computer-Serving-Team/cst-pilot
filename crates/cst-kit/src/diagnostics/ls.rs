use super::scan::{ScanSnapshot, format_bytes};
use super::volume_index::{VolumeIndex, display_path, is_volume_root};
use super::walk::{
    DirectChild, DirectChildKind, DirectFileSeed, WalkBudget, WalkLimits, linked,
    list_direct_children, measure_ls_subdirs,
};
use super::win_native::{Control, rounded};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

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
    index: &VolumeIndex,
    target: &Path,
    top: usize,
    control: &Control,
    limits: WalkLimits,
) -> Result<Value, String> {
    let meta = fs::symlink_metadata(target).map_err(|_| "目标目录无法读取。")?;
    if linked(&meta) || !meta.is_dir() {
        return Err("目标必须是目录且不能是链接或重解析点。".into());
    }
    let mut budget = WalkBudget::new(limits);

    // Use warm volume snapshot only when allowed by the index (disabled by default in production
    // until Stage 1C live-volume gates pass, and always bypassed when `CST_DISABLE_MFT=1`).
    let snapshot = if !index.allows_mft() {
        None
    } else if let Some(cached) = index.cached(target) {
        Some(cached)
    } else if is_volume_root(target) {
        let scan_control = Control::new(
            control.cancel.child_token(),
            std::time::Duration::from_secs(180).min(control.remaining()),
        );
        match index.scan(target, false, &scan_control) {
            Ok(snapshot) => snapshot,
            Err(_) => {
                control.check()?;
                None
            }
        }
    } else {
        None
    };

    // Single call-level budget starts before enumerating direct children.
    let page = list_direct_children(target, &mut budget, control)?;
    measure(
        index,
        target,
        top,
        control,
        &mut budget,
        snapshot.as_deref(),
        page.entries,
        page.truncated,
    )
}

#[allow(clippy::too_many_arguments)]
fn measure(
    index: &VolumeIndex,
    target: &Path,
    top: usize,
    control: &Control,
    budget: &mut WalkBudget,
    snapshot: Option<&ScanSnapshot>,
    entries: impl IntoIterator<Item = Result<DirectChild, ()>>,
    children_truncated: bool,
) -> Result<Value, String> {
    let mut direct_children: Vec<DirectChild> = Vec::new();
    for entry in entries {
        let Ok(child) = entry else {
            budget.incomplete = true;
            continue;
        };
        direct_children.push(child);
    }

    // Decide cleanly between pure `native-index` (every direct subdirectory resolved from one
    // coherent volume snapshot) vs pure `walk` (single query-scoped parallel traversal sharing
    // one hardlink dedup table across direct files and all sibling subtrees — never mixing
    // whole-volume hardlink ownership with local subtree ownership).
    let mut all_dirs_cached = snapshot.is_some();
    let mut cached_dir_sizes: Vec<Option<u64>> = vec![None; direct_children.len()];
    if let Some(snap) = snapshot {
        for (idx, child) in direct_children.iter().enumerate() {
            if child.kind == DirectChildKind::Dir {
                if let Some(bytes) = snap.lookup_dir(child.file_id, &child.path) {
                    cached_dir_sizes[idx] = Some(bytes);
                } else {
                    all_dirs_cached = false;
                }
            }
        }
    }

    let mut used_cache = false;
    let mut walked = false;
    let mut walked_dir_sizes: Vec<Option<u64>> = vec![None; direct_children.len()];

    if !all_dirs_cached {
        let mut direct_file_seeds = Vec::new();
        let mut subdir_indices = Vec::new();
        let mut subdir_paths: Vec<PathBuf> = Vec::new();
        for (idx, child) in direct_children.iter().enumerate() {
            match child.kind {
                DirectChildKind::File => {
                    if let Some(bytes) = child.bytes {
                        direct_file_seeds.push(DirectFileSeed {
                            file_id: child.file_id.unwrap_or(0),
                            bytes,
                            name: child.name.clone(),
                        });
                    }
                }
                DirectChildKind::Dir => {
                    subdir_indices.push(idx);
                    subdir_paths.push(child.path.clone());
                }
                DirectChildKind::ReparseOrCloud | DirectChildKind::Unknown => {}
            }
        }
        if !subdir_paths.is_empty() {
            walked = true;
            let measured =
                measure_ls_subdirs(target, &direct_file_seeds, &subdir_paths, budget, control)?;
            for (slot, idx) in subdir_indices.into_iter().enumerate() {
                walked_dir_sizes[idx] = measured.get(slot).copied().flatten();
                if walked_dir_sizes[idx].is_none() {
                    budget.incomplete = true;
                }
            }
        }
    }

    let mut items = Vec::with_capacity(direct_children.len());
    for (idx, child) in direct_children.into_iter().enumerate() {
        let (kind, bytes) = match child.kind {
            DirectChildKind::ReparseOrCloud => {
                budget.incomplete = true;
                ("unknown", None)
            }
            DirectChildKind::Dir => {
                if all_dirs_cached {
                    used_cache = true;
                    ("dir", cached_dir_sizes[idx])
                } else {
                    ("dir", walked_dir_sizes[idx])
                }
            }
            DirectChildKind::File => ("file", child.bytes),
            DirectChildKind::Unknown => {
                if all_dirs_cached && let Some(snap) = snapshot {
                    let cached_dir = snap.lookup_dir(child.file_id, &child.path);
                    let cached_file = snap.lookup_file(&child.path);
                    match (cached_dir, cached_file) {
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
                    }
                } else {
                    budget.incomplete = true;
                    ("unknown", None)
                }
            }
        };
        items.push(Item {
            name: child.name,
            kind,
            bytes,
        });
    }

    if used_cache && snapshot.is_some_and(|snapshot| !index.is_current(snapshot, target)) {
        return Err("目录读取期间卷身份变化，请重试。".into());
    }
    items.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    let total = items
        .iter()
        .filter_map(|i| i.bytes)
        .fold(0u64, u64::saturating_add);
    let mut result = json!({
        "path": display_path(target),
        "totalChildren": items.len(),
        "totalSize": format_bytes(total),
        "method": if used_cache && !walked { "native-index" } else { "walk" },
        "entries": items
            .iter()
            .take(top)
            .map(|i| json!({
                "name": i.name,
                "type": i.kind,
                "bytes": i.bytes,
                "size": i.bytes.map(format_bytes),
                "pct": if budget.incomplete || total == 0 {
                    None
                } else {
                    i.bytes.map(|v| rounded(v as f64 / total as f64 * 100.0, 1))
                }
            }))
            .collect::<Vec<_>>()
    });
    if items.len() > top {
        let omitted = &items[top..];
        result["omitted"] = json!({
            "count": omitted.len(),
            "unknownCount": omitted.iter().filter(|i| i.bytes.is_none()).count(),
            "size": format_bytes(
                omitted
                    .iter()
                    .filter_map(|i| i.bytes)
                    .fold(0u64, u64::saturating_add)
            ),
            "note": "已按已知大小截断，未知大小不参与排序"
        });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ls_keeps_readable_siblings_after_directory_entry_error() {
        let dir = std::env::temp_dir().join(format!("cst-ls-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("readable"), b"test").unwrap();
        let control = Control::new(Default::default(), std::time::Duration::from_secs(3));
        let mut budget = WalkBudget::new(WalkLimits::default());
        let page = list_direct_children(&dir, &mut budget, &control).unwrap();
        let entries = std::iter::once(Err(())).chain(page.entries);
        let index = VolumeIndex::new(std::sync::Arc::new(
            super::super::volume::NativeScanSource::new(),
        ));
        let result = measure(
            &index,
            &dir,
            10,
            &control,
            &mut budget,
            None,
            entries,
            page.truncated,
        );
        fs::remove_dir_all(&dir).unwrap();
        let result = result.unwrap();
        assert_eq!(result["totalChildren"], 1);
        assert_eq!(result["entries"][0]["bytes"], 4);
        assert!(result["entries"][0]["pct"].is_null());
        assert!(result["notice"].is_string());
    }
}

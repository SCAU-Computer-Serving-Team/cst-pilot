use super::volume::NativeScanSource;
use super::walk::WalkLimits;
use super::win_native::{Control, DiagnosticSource};
use super::wz_index::WzIndex;
use super::{disk, ls};
use pi_session::ToolDefinition;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub fn definitions(root: &Path, source: Arc<dyn DiagnosticSource>) -> Vec<ToolDefinition> {
    let index = Arc::new(WzIndex::new(Arc::new(NativeScanSource::new(root))));
    let ls_index = index.clone();
    let ls_source = source.clone();
    let mut ls = ToolDefinition::from_typed(
        "ls",
        "LS",
        "按大小浏览目录直接子项，附占比与统计下界。",
        move |_, input: ls::LsInput, cancel, _, ctx| {
            let index = ls_index.clone();
            let source = ls_source.clone();
            let cwd = PathBuf::from(ctx.cwd());
            async move {
                let value = super::execute(
                    source,
                    (input, cwd),
                    cancel.unwrap_or_default(),
                    Duration::from_secs(215),
                    move |_, (input, cwd), control| {
                        let target = resolve(&cwd, &input.path)?;
                        ls::run(
                            &index,
                            &target,
                            input.top.unwrap_or(20).clamp(1, 50),
                            control,
                            WalkLimits::default(),
                        )
                    },
                )
                .await?;
                super::result::diagnostic_result(value)
            }
        },
    );
    ls.prompt_snippet =
        Some("按大小浏览目录，仅统计直接子项，未知大小和预算下界在结果中说明。".into());
    let mut disk = ToolDefinition::from_typed(
        "disk",
        "Disk",
        "卷空间、物理盘、可靠性与整树占用排行。",
        move |_, input: disk::DiskInput, cancel, _, ctx| {
            let index = index.clone();
            let source = source.clone();
            let cwd = PathBuf::from(ctx.cwd());
            async move {
                let timeout =
                    Duration::from_secs(if matches!(input.scope, disk::DiskScope::Usage) {
                        215
                    } else {
                        30
                    });
                let value = super::execute(
                    source,
                    (input, cwd),
                    cancel.unwrap_or_default(),
                    timeout,
                    move |source, (input, cwd), control| {
                        disk::normalize_drive(input.drive.as_deref())?;
                        if !matches!(input.scope, disk::DiskScope::Usage) {
                            return disk::run(source, input, control);
                        }
                        let target =
                            resolve(&cwd, input.path.as_deref().ok_or("usage 需要提供 path。")?)?;
                        let top = input.top.unwrap_or(20).clamp(1, 100);
                        let scan = Control::new(
                            control.cancel.child_token(),
                            Duration::from_secs(180).min(control.remaining()),
                        );
                        let fast = index.scan(&target, true, &scan);
                        if control.cancel.is_cancelled() {
                            control.check()?;
                        }
                        let usage = match fast {
                            Ok(Some(snapshot)) => {
                                if !index.is_current(&snapshot, &target) {
                                    return Err("扫描期间卷身份变化，请重试。".into());
                                }
                                snapshot.usage(
                                    &target,
                                    top,
                                    snapshot.identity.as_ref().map(|id| id.filesystem.as_str()),
                                )
                            }
                            other => {
                                let mut value = super::walk::usage(
                                    &target,
                                    top,
                                    control,
                                    WalkLimits::default(),
                                )?;
                                if let Err(error) = other {
                                    value["degradedFrom"] =
                                        serde_json::json!(format!("wiztree: {error}"));
                                }
                                value
                            }
                        };
                        Ok(serde_json::json!({"usage":usage}))
                    },
                )
                .await?;
                super::result::diagnostic_result(value)
            }
        },
    );
    disk.prompt_snippet =
        Some("查询磁盘空间、设备、SMART 或目录树占用；usage 扫描与 ls 共享卷缓存。".into());
    vec![ls, disk]
}
fn resolve(cwd: &Path, input: &str) -> Result<PathBuf, String> {
    if input.trim().is_empty() {
        return Err("目录路径不能为空。".into());
    }
    let target = cwd.join(input);
    let meta = std::fs::symlink_metadata(&target).map_err(|_| "目标目录无法读取。")?;
    if super::walk::linked(&meta) || !meta.is_dir() {
        return Err("目标必须是目录且不能是链接或重解析点。".into());
    }
    target
        .canonicalize()
        .map_err(|_| "目标目录无法解析。".into())
}

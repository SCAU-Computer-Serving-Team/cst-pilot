use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pi_agent::BoxFuture;

use crate::discovery::strip_windows_verbatim_prefix;
use crate::error::{ServiceError, ServiceErrorKind};
use crate::exports::snapshot::validate_artifact_date;

pub type ExportDirectoryOpenerFn =
    Arc<dyn Fn(PathBuf) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

pub fn default_export_directory_opener() -> ExportDirectoryOpenerFn {
    Arc::new(|directory: PathBuf| {
        Box::pin(async move {
            #[cfg(windows)]
            {
                let system_root =
                    std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
                let explorer = PathBuf::from(system_root).join("explorer.exe");
                tokio::process::Command::new(explorer)
                    .arg(&directory)
                    .spawn()
                    .map(|_| ())
                    .map_err(|_| "无法启动文件管理器".to_owned())
            }
            #[cfg(not(windows))]
            {
                let _ = directory;
                Err("当前系统不支持本地目录打开".to_owned())
            }
        })
    })
}

fn is_reparse_or_symlink(meta: &fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        if (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
            return true;
        }
    }
    false
}

fn map_dir_io_error(err: &io::Error, create: bool) -> ServiceError {
    match err.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory if !create => ServiceError::new(
            ServiceErrorKind::NotFound,
            "export_directory_missing",
            "保存目录已不存在，请重新导出",
        ),
        io::ErrorKind::PermissionDenied => ServiceError::new(
            ServiceErrorKind::AccessDenied,
            "export_directory_denied",
            "无法访问保存目录，请检查目录权限",
        ),
        _ if err.raw_os_error() == Some(112) => ServiceError::new(
            ServiceErrorKind::StorageFull,
            "storage_full",
            "工具包存储空间不足，请清理后重试",
        ),
        _ if create => ServiceError::new(
            ServiceErrorKind::Internal,
            "export_failed",
            "导出未完成，请检查浏览器和工具包写入权限后重试",
        ),
        _ => ServiceError::new(
            ServiceErrorKind::Unavailable,
            "export_directory_unavailable",
            "保存目录无法读取，请检查工具包所在介质",
        ),
    }
}

fn checked_artifact_directory(
    root: &Path,
    day: &str,
    kind: &str,
    create: bool,
) -> Result<PathBuf, ServiceError> {
    let valid_day = validate_artifact_date(day)?;
    let resolved_root = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(root)
    };

    if create {
        fs::create_dir_all(&resolved_root).map_err(|e| map_dir_io_error(&e, true))?;
    }

    let root_meta =
        fs::symlink_metadata(&resolved_root).map_err(|e| map_dir_io_error(&e, create))?;
    if is_reparse_or_symlink(&root_meta) {
        return Err(ServiceError::new(
            ServiceErrorKind::AccessDenied,
            "export_directory_invalid",
            "保存目录指向工具包外，无法打开",
        ));
    }
    if !root_meta.is_dir() {
        return Err(if create {
            ServiceError::new(
                ServiceErrorKind::Internal,
                "export_failed",
                "导出未完成，请检查浏览器和工具包写入权限后重试",
            )
        } else {
            ServiceError::new(
                ServiceErrorKind::NotFound,
                "export_directory_missing",
                "保存目录已不存在，请重新导出",
            )
        });
    }

    let base = fs::canonicalize(&resolved_root)
        .map(|p| strip_windows_verbatim_prefix(&p))
        .map_err(|e| map_dir_io_error(&e, create))?;
    if let Some(parent) = resolved_root.parent()
        && let Ok(canon_parent) =
            fs::canonicalize(parent).map(|p| strip_windows_verbatim_prefix(&p))
    {
        let expected_name = resolved_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let actual_rel = base.strip_prefix(&canon_parent).ok();
        let actual_name = actual_rel.and_then(|p| p.to_str()).unwrap_or("");
        let matches = if cfg!(windows) {
            actual_name.eq_ignore_ascii_case(expected_name)
        } else {
            actual_name == expected_name
        };
        if !matches {
            return Err(ServiceError::new(
                ServiceErrorKind::AccessDenied,
                "export_directory_invalid",
                "保存目录指向工具包外，无法打开",
            ));
        }
    }

    let mut dir = resolved_root;
    for part in [valid_day.as_str(), kind] {
        dir = dir.join(part);
        if create {
            fs::create_dir_all(&dir).map_err(|e| map_dir_io_error(&e, true))?;
        }
        let meta = fs::symlink_metadata(&dir).map_err(|e| map_dir_io_error(&e, create))?;
        if is_reparse_or_symlink(&meta) {
            return Err(ServiceError::new(
                ServiceErrorKind::AccessDenied,
                "export_directory_invalid",
                "保存目录指向工具包外，无法打开",
            ));
        }
        if !meta.is_dir() {
            return Err(ServiceError::new(
                ServiceErrorKind::NotFound,
                "export_directory_missing",
                "保存目录已不存在，请重新导出",
            ));
        }
        let real = fs::canonicalize(&dir)
            .map(|p| strip_windows_verbatim_prefix(&p))
            .map_err(|e| map_dir_io_error(&e, create))?;
        if !real.starts_with(&base) || real == base {
            return Err(ServiceError::new(
                ServiceErrorKind::AccessDenied,
                "export_directory_invalid",
                "保存目录指向工具包外，无法打开",
            ));
        }
    }

    Ok(dir)
}

pub fn artifact_sessions_display_path(root: &Path, day: &str) -> Result<PathBuf, ServiceError> {
    let valid_day = validate_artifact_date(day)?;
    Ok(root.join(valid_day).join("sessions"))
}

pub fn ensure_artifact_sessions_dir(root: &Path, day: &str) -> Result<PathBuf, ServiceError> {
    checked_artifact_directory(root, day, "sessions", true)
}

pub fn existing_artifact_sessions_dir(root: &Path, day: &str) -> Result<PathBuf, ServiceError> {
    let dir = checked_artifact_directory(root, day, "sessions", false)?;
    let real = fs::canonicalize(&dir)
        .map(|p| strip_windows_verbatim_prefix(&p))
        .map_err(|e| map_dir_io_error(&e, false))?;
    Ok(real)
}

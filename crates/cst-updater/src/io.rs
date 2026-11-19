use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use cst_os::{DirectoryLease, Storage};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

pub fn cancelled(cancel: &CancellationToken) -> Result<(), String> {
    if cancel.is_cancelled() {
        Err("升级已取消。".into())
    } else {
        Ok(())
    }
}

pub fn atomic(
    storage: &Storage,
    path: &Path,
    bytes: &[u8],
    replace: bool,
    cancel: &CancellationToken,
) -> Result<(), String> {
    storage.atomic_checked(path, bytes, replace, &|| cancel.is_cancelled())
}

pub fn hash(
    storage: &Storage,
    path: &Path,
    cancel: &CancellationToken,
) -> Result<Option<String>, String> {
    cancelled(cancel)?;
    let Some(mut file) = storage.open_read(path)? else {
        return Ok(None);
    };
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0; 65536];
    loop {
        cancelled(cancel)?;
        let n = file.read(&mut buffer).map_err(|_| "升级文件无法读取。")?;
        if n == 0 {
            break;
        }
        total = total
            .checked_add(n as u64)
            .filter(|n| *n <= 8 * 1024 * 1024 * 1024)
            .ok_or("升级文件超过大小限制。")?;
        hash.update(&buffer[..n]);
    }
    Ok(Some(format!("{:x}", hash.finalize())))
}

pub fn same(
    storage: &Storage,
    path: &Path,
    expected: Option<&str>,
    cancel: &CancellationToken,
) -> Result<(), String> {
    if hash(storage, path, cancel)?.as_deref() != expected {
        return Err("升级文件已发生变化，保留原文件及备份供核对。".into());
    }
    Ok(())
}

pub fn parent_pin(storage: &Storage, path: &Path, create: bool) -> Result<DirectoryLease, String> {
    let relative = path
        .parent()
        .and_then(|p| p.strip_prefix(&storage.root).ok())
        .ok_or("升级文件越界。")?;
    storage.pin_directory(relative, create)
}

pub fn move_file(
    storage: &Storage,
    from: &Path,
    to: &Path,
    expected: &str,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let _from = parent_pin(storage, from, false)?;
    let _to = parent_pin(storage, to, true)?;
    same(storage, from, Some(expected), cancel)?;
    same(storage, to, None, cancel)?;
    for attempt in 0..50 {
        cancelled(cancel)?;
        match rename(from, to) {
            Ok(()) => return same(storage, to, Some(expected), cancel),
            Err(error) if matches!(error.raw_os_error(), Some(5 | 32 | 33)) && attempt < 49 => {
                std::thread::sleep(Duration::from_millis(20 + attempt * 5));
            }
            Err(_) => return Err("升级文件移动失败，事务与备份已保留。".into()),
        }
    }
    Err("升级文件仍被占用。".into())
}

fn rename(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::{
            Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW},
            core::PCWSTR,
        };
        let from: Vec<_> = from.as_os_str().encode_wide().chain([0]).collect();
        let to: Vec<_> = to.as_os_str().encode_wide().chain([0]).collect();
        unsafe {
            MoveFileExW(
                PCWSTR(from.as_ptr()),
                PCWSTR(to.as_ptr()),
                MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(std::io::Error::from)
    }
    #[cfg(not(windows))]
    {
        fs::hard_link(from, to)?;
        fs::remove_file(from)
    }
}

pub fn remove(
    storage: &Storage,
    path: &Path,
    expected: &str,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let _pin = parent_pin(storage, path, false)?;
    same(storage, path, Some(expected), cancel)?;
    fs::remove_file(path).map_err(|_| "升级文件无法移除，原事务已保留。".into())
}

pub fn clean(storage: &Storage, relative: &Path) -> Result<(), String> {
    match fs::symlink_metadata(storage.root.join(relative)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("升级临时目录无法检查。".into()),
        Ok(_) => crate::verify::remove_stage(storage, relative),
    }
}

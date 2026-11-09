use chrono::Local;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

use super::RunbookStore;
use super::model::{RunbookInput, artifact_date, build_body, validate};
use super::storage::{cancelled, io_error};

/// The pipe name matches the existing TypeScript writer. The kernel releases it
/// on exit; FAT/exFAT volumes do not need persistent lock files.
pub struct DirectoryLock {
    #[cfg(windows)]
    _handle: std::os::windows::io::OwnedHandle,
    #[cfg(not(windows))]
    _listener: std::net::TcpListener,
}
impl DirectoryLock {
    pub fn acquire(directory: &Path) -> Result<Self, String> {
        let canonical = directory
            .canonicalize()
            .map_err(|e| io_error("解析清单锁目录", e))?;
        let name = canonical.to_string_lossy().into_owned();
        #[cfg(windows)]
        let name = {
            // Node realpath uses ordinary drive/UNC names rather than Rust's verbatim prefix.
            let name = if let Some(unc) = name.strip_prefix(r"\\?\UNC\") {
                format!(r"\\{unc}")
            } else {
                name.strip_prefix(r"\\?\").unwrap_or(&name).to_owned()
            };
            name.to_lowercase()
        };
        let key = Sha256::digest(name.as_bytes());
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows::{
                Win32::{
                    Foundation::INVALID_HANDLE_VALUE,
                    Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX},
                    System::Pipes::{
                        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
                        PIPE_TYPE_BYTE, PIPE_WAIT,
                    },
                },
                core::PCWSTR,
            };
            let name = format!(r"\\.\pipe\cst-pilot-runbook-{key:x}");
            let wide: Vec<_> = std::ffi::OsStr::new(&name)
                .encode_wide()
                .chain(Some(0))
                .collect();
            let handle = unsafe {
                CreateNamedPipeW(
                    PCWSTR(wide.as_ptr()),
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    0,
                    0,
                    0,
                    None,
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                let e = std::io::Error::last_os_error();
                return Err(if matches!(e.raw_os_error(), Some(5 | 231)) {
                    "清单目录正由另一进程写入，请稍后重试。".into()
                } else {
                    io_error("建立清单目录锁", e)
                });
            }
            use std::os::windows::io::FromRawHandle;
            // The pipe is not thread-affine. OwnedHandle supplies Send and closes it once.
            Ok(Self {
                _handle: unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle.0) },
            })
        }
        #[cfg(not(windows))]
        {
            let prefix = u32::from_be_bytes(key[..4].try_into().map_err(|_| "目录锁摘要无效。")?);
            let listener = std::net::TcpListener::bind((
                std::net::Ipv4Addr::LOCALHOST,
                32000 + (prefix % 20000) as u16,
            ))
            .map_err(|_| "清单目录正由另一进程写入，请稍后重试。")?;
            Ok(Self {
                _listener: listener,
            })
        }
    }
}

impl RunbookStore {
    pub async fn write(
        &self,
        input: RunbookInput,
        cancel: CancellationToken,
    ) -> Result<Value, String> {
        let (title, items) = validate(&input.title, &input.items)?;
        if input.sequence.is_some_and(|n| !(1..=999).contains(&n)) {
            return Err("sequence 必须是 1–999 的整数。".into());
        }
        let day = artifact_date(input.date.as_deref())?;
        let relative = PathBuf::from("artifacts").join(&day).join("runbook");
        self.locked(relative, cancel, move |storage, dir, cancel| {
            let mut entries = BTreeMap::new();
            for entry in fs::read_dir(dir).map_err(|e| io_error("读取清单目录", e))? {
                cancelled(cancel)?;
                let entry = entry.map_err(|e| io_error("读取清单文件名", e))?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Some(sequence) = sequence(&name) {
                    // Reject links and duplicate slots even when they are not the requested one.
                    storage.read(&entry.path())?.ok_or("清单在读取时消失。")?;
                    if entries.insert(sequence, entry.path()).is_some() {
                        return Err(format!("序号 {sequence} 已重复，请检查清单目录。"));
                    }
                }
            }
            let sequence = input.sequence.unwrap_or_else(|| entries.keys().next_back().copied().unwrap_or(0) + 1);
            if sequence > 999 {
                return Err("当天序号已达到 999，请在次日生成新清单。".into());
            }
            let previous = entries.get(&sequence);
            if input.sequence.is_some() && previous.is_none() {
                return Err(format!("序号 {sequence} 不存在，无法修正。"));
            }
            let name = format!("{sequence:02}-{}-{title}.txt", input.level.label());
            let file = dir.join(&name);
            let same = previous.is_some_and(|old| same_name(old, &file));
            let body = build_body(&title, input.level, &items, &Local::now().format("%Y-%m-%d %H:%M").to_string());
            storage.atomic(&file, &body, same, cancel)?;
            if let Some(old) = previous.filter(|_| !same) {
                // The new name is committed first. A failed cleanup leaves the original intact.
                if let Err(e) = fs::remove_file(old) {
                    fs::remove_file(&file).map_err(|rollback| format!("旧清单无法删除：{e}；新文件回收失败：{rollback}。请检查清单目录。"))?;
                    return Err(io_error("清理原清单文件名", e));
                }
            }
            let commands: Vec<_> = items.iter().map(|item| json!({
                "summary":item.summary,"command":item.command,"shell":item.shell.label(),"admin":item.admin
            })).collect();
            Ok(json!({"runbook":{
                "title":title,"file":file,"dir":dir,"name":name,"sequence":sequence,
                "action":if input.sequence.is_some(){"replaced"}else{"created"},
                "level":input.level,"levelLabel":input.level.label(),"items":items.len(),"commands":commands,
                "bytes":body.len(),"encoding":"utf-8-bom",
                "notice":format!("清单只写工具包 artifacts/<日期>/runbook，未改动机主系统。本次序号 {sequence}。修正时沿用 sequence={sequence}。文件是纯文本，不会自动执行；请原样转述 file 路径，队员打开后逐条复制命令。")
            }}))
        }).await
    }
}
fn sequence(name: &str) -> Option<u16> {
    let (number, rest) = name.split_once('-')?;
    if !(2..=3).contains(&number.len()) || !number.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (level, title) = rest.split_once('-')?;
    if !matches!(level, "安全" | "较安全" | "需确认") || title.strip_suffix(".txt")?.is_empty()
    {
        return None;
    }
    number.parse::<u16>().ok().filter(|n| *n >= 1)
}
fn same_name(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

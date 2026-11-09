use super::{
    layout,
    manifest::{MAX_FILES, ReleaseKey, VerifiedRelease},
    network::Fetcher,
};
use crate::runbook::storage::Storage;
use futures_util::StreamExt;
use regex::bytes::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
use zip::ZipArchive;

pub struct VerifiedPackage {
    pub(crate) stage: Stage,
    pub(crate) release: VerifiedRelease,
}
impl VerifiedPackage {
    pub fn directory(&self) -> PathBuf {
        self.stage.root.join("program")
    }
    pub fn release(&self) -> &VerifiedRelease {
        &self.release
    }
    /// Call again immediately before handing the package to the independent updater.
    pub fn revalidate(&self, key: &ReleaseKey, cancel: &CancellationToken) -> Result<(), String> {
        let _pin = self
            .stage
            .base
            .pin_directory(&self.stage.relative.join("program"), false)?;
        let release = key.verify(self.release.envelope())?;
        verify_tree(&self.directory(), &release, cancel)
    }
}
pub(crate) struct Stage {
    pub root: PathBuf,
    pub(super) retain: bool,
    base: Storage,
    relative: PathBuf,
}
impl Drop for Stage {
    fn drop(&mut self) {
        if !self.retain {
            let _ = remove_stage(&self.base, &self.relative);
        }
    }
}
fn stopped(cancel: &CancellationToken) -> Result<(), String> {
    if cancel.is_cancelled() {
        Err("升级准备已取消。".into())
    } else {
        Ok(())
    }
}
pub(crate) async fn prepare(
    root: &Path,
    release: VerifiedRelease,
    fetcher: &dyn Fetcher,
    cancel: CancellationToken,
) -> Result<VerifiedPackage, String> {
    stopped(&cancel)?;
    let required = release
        .info
        .archive
        .unpacked_size
        .checked_mul(2)
        .and_then(|n| n.checked_add(release.info.archive.size))
        .ok_or("升级空间需求无效。")?;
    if layout::available_space(root)? < required {
        return Err("工具包所在卷剩余空间不足。".into());
    }
    let storage = Storage::new(root)?;
    let parent = Path::new(".state/update/staging");
    let _parent_pin = storage.pin_directory(parent, true)?;
    let stage_path = storage.root.join(parent).join(format!(
        "{}-{}",
        release.info.version,
        uuid::Uuid::new_v4()
    ));
    fs::create_dir(&stage_path).map_err(|_| "升级暂存目录不可写。")?;
    let stage = Stage {
        retain: false,
        relative: stage_path
            .strip_prefix(&storage.root)
            .map_err(|_| "暂存目录越界。")?
            .to_owned(),
        root: stage_path,
        base: storage.clone(),
    };
    let pin = storage.pin_directory(&stage.relative, false)?;
    let local = Storage::new(&stage.root)?;
    let archive = stage.root.join("package.zip");
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&archive)
        .map_err(|_| "无法创建升级下载文件。")?;
    let mut file = tokio::fs::File::from_std(file);
    let download = async {
        let mut stream = fetcher
            .get(
                release.info.archive.url.clone(),
                release.info.archive.size,
                cancel.clone(),
            )
            .await
            .map_err(|_| "升级包下载失败。")?;
        let mut count = 0u64;
        let mut hash = Sha256::new();
        while let Some(chunk) = stream.body.next().await {
            stopped(&cancel)?;
            let chunk = chunk.map_err(|_| "升级包下载失败。")?;
            count = count
                .checked_add(chunk.len() as u64)
                .filter(|n| *n <= release.info.archive.size)
                .ok_or("升级包超过签名声明的体积。")?;
            file.write_all(&chunk)
                .await
                .map_err(|_| "写入升级包失败。")?;
            hash.update(&chunk);
        }
        file.sync_all().await.map_err(|_| "升级包同步失败。")?;
        if count != release.info.archive.size
            || format!("{:x}", hash.finalize()) != release.info.archive.sha256
        {
            return Err("升级包大小或 SHA-256 不匹配。".into());
        }
        Ok(())
    };
    let result: Result<(), String> = tokio::select! { biased;
        _ = cancel.cancelled() => Err("升级包下载已取消。".into()),
        r = tokio::time::timeout(Duration::from_secs(10), download) => r.map_err(|_| "升级包下载超过 10 秒。".to_owned())?,
    };
    drop(file);
    result?;
    local.atomic(
        &stage.root.join("manifest.json"),
        release.envelope(),
        false,
        &cancel,
    )?;
    drop(pin);
    let child = cancel.child_token();
    let _cancel_on_drop = child.clone().drop_guard();
    tokio::task::spawn_blocking(move || {
        let _stage_pin = stage.base.pin_directory(&stage.relative, false)?;
        let program = local.directory(Path::new("program"), true)?;
        extract(&archive, &program, &release, &child)?;
        verify_tree(&program, &release, &child)?;
        Ok(VerifiedPackage { stage, release })
    })
    .await
    .map_err(|_| "升级解压任务失败。".to_owned())?
}
fn extract(
    archive: &Path,
    root: &Path,
    release: &VerifiedRelease,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let parent = Storage::new(archive.parent().ok_or("升级文件路径无效。")?)?;
    let _parent_pin = parent.pin_directory(Path::new(""), false)?;
    let mut file = parent.open_read(archive)?.ok_or("升级包不存在。")?;
    zip_bounds(&mut file)?;
    let mut archive = ZipArchive::new(file).map_err(|_| "升级包 ZIP 格式无效。")?;
    if archive.len() > MAX_FILES * 2 {
        return Err("升级归档条目过多。".into());
    }
    let storage = Storage::new(root)?;
    let mut seen = BTreeSet::new();
    let mut spellings = BTreeMap::new();
    let mut count = 0u64;
    let sums_size = release.info.sha256_sums.len() as u64;
    for i in 0..archive.len() {
        stopped(cancel)?;
        let mut file = archive.by_index(i).map_err(|_| "升级归档条目无法读取。")?;
        let raw =
            std::str::from_utf8(file.name_raw()).map_err(|_| "升级归档文件名必须是 UTF-8。")?;
        if raw != file.name() {
            return Err("升级归档文件名编码存在歧义。".into());
        }
        let directory = file.is_dir();
        let raw = if directory {
            raw.strip_suffix('/').unwrap_or(raw)
        } else {
            raw
        };
        let Some(relative) = raw.strip_prefix("cst-pilot/") else {
            if raw == "cst-pilot" && directory {
                continue;
            }
            return Err("升级归档必须仅包含 cst-pilot/ 顶层目录。".into());
        };
        layout::safe_path(relative)?;
        let relative = relative.to_owned();
        if !seen.insert(relative.to_lowercase()) {
            return Err("升级归档有重复或大小写冲突。".into());
        }
        check_spelling(&relative, &mut spellings)?;
        let mode = file
            .unix_mode()
            .unwrap_or(if directory { 0o040000 } else { 0o100000 })
            & 0o170000;
        if file.encrypted()
            || file.is_symlink()
            || (mode != 0 && mode != if directory { 0o040000 } else { 0o100000 })
        {
            return Err("升级归档包含加密文件、链接或特殊文件。".into());
        }
        check_extra(file.extra_data().unwrap_or_default())?;
        if directory {
            if file.size() != 0
                || !release
                    .sums
                    .keys()
                    .any(|p| p.starts_with(&format!("{relative}/")))
            {
                return Err("升级归档目录不在签名清单中。".into());
            }
            let _pin = storage.pin_directory(Path::new(&relative), true)?;
            continue;
        }
        layout::classify(&relative)?;
        let expected = release.sums.get(&relative);
        if expected.is_none() && relative != "SHA256SUMS" {
            return Err("升级归档含未列入签名清单的文件。".into());
        }
        let limit = if relative == "SHA256SUMS" {
            sums_size
        } else {
            release.info.archive.unpacked_size
        };
        if file.size() > limit {
            return Err("升级文件体积超过限制。".into());
        }
        count = count
            .checked_add(file.size())
            .filter(|n| *n <= release.info.archive.unpacked_size)
            .ok_or("升级解压总体积超限。")?;
        let path = root.join(&relative);
        let _pin =
            storage.pin_directory(Path::new(&relative).parent().unwrap_or(Path::new("")), true)?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|_| "无法创建升级程序文件。")?;
        let mut hash = Sha256::new();
        let mut read = 0u64;
        let mut buffer = [0; 64 * 1024];
        loop {
            stopped(cancel)?;
            let n = file
                .read(&mut buffer)
                .map_err(|_| "升级解压或 CRC 校验失败。")?;
            if n == 0 {
                break;
            }
            read = read
                .checked_add(n as u64)
                .filter(|n| *n <= limit)
                .ok_or("升级文件解压体积超限。")?;
            hash.update(&buffer[..n]);
            output
                .write_all(&buffer[..n])
                .map_err(|_| "升级文件写入失败。")?;
        }
        if read != file.size() || expected.is_some_and(|v| *v != format!("{:x}", hash.finalize())) {
            return Err("升级文件哈希或体积不匹配。".into());
        }
        output.sync_all().map_err(|_| "升级文件同步失败。")?;
    }
    if count != release.info.archive.unpacked_size {
        return Err("升级解压体积与签名清单不一致。".into());
    }
    Ok(())
}
fn zip_bounds(file: &mut File) -> Result<(), String> {
    let error = || "升级 ZIP 的目录结构无效或超过资源限制。".to_owned();
    let len = file.metadata().map_err(|_| error())?.len();
    let start = len.saturating_sub(65_557);
    file.seek(SeekFrom::Start(start)).map_err(|_| error())?;
    let mut tail = Vec::new();
    (&mut *file)
        .take(65_557)
        .read_to_end(&mut tail)
        .map_err(|_| error())?;
    let at = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| {
            tail[i..].starts_with(b"PK\x05\x06")
                && i + 22 + usize::from(u16::from_le_bytes([tail[i + 20], tail[i + 21]]))
                    == tail.len()
        })
        .ok_or_else(error)?;
    let eocd = &tail[at..];
    if eocd[4..8] != [0, 0, 0, 0] || eocd[8..10] != eocd[10..12] {
        return Err(error());
    }
    let mut count = u64::from(u16::from_le_bytes(
        eocd[10..12].try_into().map_err(|_| error())?,
    ));
    let mut size = u64::from(u32::from_le_bytes(
        eocd[12..16].try_into().map_err(|_| error())?,
    ));
    let mut offset = u64::from(u32::from_le_bytes(
        eocd[16..20].try_into().map_err(|_| error())?,
    ));
    if count == 0xffff || size == 0xffff_ffff || offset == 0xffff_ffff {
        if at < 20 {
            return Err(error());
        }
        let locator = &tail[at - 20..at];
        if &locator[..8] != b"PK\x06\x07\0\0\0\0" || locator[16..] != [1, 0, 0, 0] {
            return Err(error());
        }
        let pos = u64::from_le_bytes(locator[8..16].try_into().map_err(|_| error())?);
        if pos
            .checked_add(56)
            .is_none_or(|n| n > start + at as u64 - 20)
        {
            return Err(error());
        }
        file.seek(SeekFrom::Start(pos)).map_err(|_| error())?;
        let mut zip64 = [0; 56];
        file.read_exact(&mut zip64).map_err(|_| error())?;
        if &zip64[..4] != b"PK\x06\x06" || zip64[16..24] != [0; 8] || zip64[24..32] != zip64[32..40]
        {
            return Err(error());
        }
        let record_size = u64::from_le_bytes(zip64[4..12].try_into().map_err(|_| error())?);
        if !(44..=4096).contains(&record_size)
            || pos.checked_add(12 + record_size) != Some(start + at as u64 - 20)
        {
            return Err(error());
        }
        count = u64::from_le_bytes(zip64[32..40].try_into().map_err(|_| error())?);
        size = u64::from_le_bytes(zip64[40..48].try_into().map_err(|_| error())?);
        offset = u64::from_le_bytes(zip64[48..56].try_into().map_err(|_| error())?);
    }
    if count == 0
        || count > (MAX_FILES * 2) as u64
        || size > 64 * 1024 * 1024
        || offset
            .checked_add(size)
            .is_none_or(|n| n > start + at as u64)
    {
        return Err(error());
    }
    file.seek(SeekFrom::Start(0)).map_err(|_| error())?;
    Ok(())
}
fn check_spelling(path: &str, names: &mut BTreeMap<String, String>) -> Result<(), String> {
    let mut prefix = String::new();
    for part in path.split('/') {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(part);
        if let Some(old) = names.insert(prefix.to_lowercase(), prefix.clone())
            && old != prefix
        {
            return Err("升级路径祖先存在大小写冲突。".into());
        }
    }
    Ok(())
}
fn check_extra(mut extra: &[u8]) -> Result<(), String> {
    while !extra.is_empty() {
        if extra.len() < 4 {
            return Err("升级 ZIP 扩展字段无效。".into());
        }
        let id = u16::from_le_bytes([extra[0], extra[1]]);
        let len = u16::from_le_bytes([extra[2], extra[3]]) as usize;
        if !matches!(id, 0x0001 | 0x5455 | 0x7875 | 0x000a) || len > extra.len() - 4 {
            return Err("升级 ZIP 包含未支持的链接或扩展元数据。".into());
        }
        extra = &extra[4 + len..];
    }
    Ok(())
}
pub(crate) fn verify_tree(
    root: &Path,
    release: &VerifiedRelease,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let storage = Storage::new(root)?;
    let mut seen = BTreeSet::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        stopped(cancel)?;
        let _pin = storage.pin_directory(&relative, false)?;
        for item in fs::read_dir(storage.root.join(&relative)).map_err(|_| "无法核验升级目录。")?
        {
            stopped(cancel)?;
            let item = item.map_err(|_| "无法核验升级目录。")?;
            let rel = relative.join(item.file_name());
            let name = rel.to_str().ok_or("升级文件名无效。")?.replace('\\', "/");
            let meta = fs::symlink_metadata(item.path()).map_err(|_| "无法核验升级文件。")?;
            if linked(&meta) {
                return Err("升级目录包含链接或重解析点。".into());
            }
            if meta.is_dir() {
                if !release
                    .sums
                    .keys()
                    .any(|p| p.starts_with(&format!("{name}/")))
                {
                    return Err("升级目录包含清单外目录。".into());
                }
                pending.push(rel);
            } else {
                layout::classify(&name)?;
                if !seen.insert(name.clone()) {
                    return Err("升级目录文件重复。".into());
                }
                let mut file = storage.open_read(&item.path())?.ok_or("升级文件消失。")?;
                if name == "SHA256SUMS" {
                    let mut bytes = Vec::new();
                    file.take(release.info.sha256_sums.len() as u64 + 1)
                        .read_to_end(&mut bytes)
                        .map_err(|_| "无法读取校验清单。")?;
                    if bytes != release.info.sha256_sums.as_bytes() {
                        return Err("包内校验清单与签名内容不一致。".into());
                    }
                } else {
                    let expected = release.sums.get(&name).ok_or("升级目录包含清单外文件。")?;
                    let mut hash = Sha256::new();
                    let mut bytes = 0u64;
                    let mut buffer = [0; 64 * 1024];
                    let mut secret_scan = SecretScan::default();
                    loop {
                        stopped(cancel)?;
                        let n = file.read(&mut buffer).map_err(|_| "无法核验升级文件。")?;
                        if n == 0 {
                            break;
                        }
                        bytes = bytes
                            .checked_add(n as u64)
                            .filter(|n| *n <= release.info.archive.unpacked_size)
                            .ok_or("升级文件大小超限。")?;
                        hash.update(&buffer[..n]);
                        secret_scan.check(&buffer[..n])?;
                    }
                    if format!("{:x}", hash.finalize()) != *expected {
                        return Err("升级目录文件哈希不匹配。".into());
                    }
                    if name.ends_with(".json") {
                        let bytes = storage
                            .read_limited(&item.path(), 8 * 1024 * 1024)?
                            .ok_or("升级 JSON 文件消失。")?;
                        let value = super::json::parse(&bytes)?;
                        check_json(&value)?;
                    }
                }
            }
        }
    }
    if seen.len() != release.sums.len() + 1
        || !seen.contains("SHA256SUMS")
        || !release.sums.keys().all(|p| seen.contains(p))
    {
        return Err("升级包缺少签名清单中的文件。".into());
    }
    layout::validate_metadata(&release.sums, &storage.root, &release.info.version)
}
#[derive(Default)]
struct SecretScan {
    tail: Vec<u8>,
}
impl SecretScan {
    fn check(&mut self, bytes: &[u8]) -> Result<(), String> {
        static PATTERN: OnceLock<Regex> = OnceLock::new();
        let pattern = PATTERN.get_or_init(|| Regex::new(r"(?-u:\b(?:sk-(?:proj-|ant-api[0-9]+-)?[A-Za-z0-9_-]{20,}|gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,}|AIza[0-9A-Za-z_-]{30,}|AKIA[0-9A-Z]{16})|eyJ[A-Za-z0-9_-]{15,}\.[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}|-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----)").expect("fixed credential pattern"));
        self.tail.extend_from_slice(bytes);
        if pattern.is_match(&self.tail) {
            return Err("升级包包含疑似凭据或私钥，已拒绝载入。".into());
        }
        if self.tail.len() > 512 {
            self.tail.drain(..self.tail.len() - 512);
        }
        Ok(())
    }
}
fn check_json(value: &Value) -> Result<(), String> {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                let key = key.to_lowercase().replace('_', "");
                if matches!(
                    key.as_str(),
                    "apikey"
                        | "accesstoken"
                        | "refreshtoken"
                        | "token"
                        | "secret"
                        | "password"
                        | "clientsecret"
                        | "authorization"
                        | "identitykey"
                ) && value.as_str().is_some_and(|v| !v.trim().is_empty())
                {
                    return Err("升级 JSON 包含非空凭据字段。".into());
                }
                check_json(value)?;
            }
        }
        Value::Array(items) => {
            for value in items {
                check_json(value)?;
            }
        }
        _ => (),
    }
    Ok(())
}
fn linked(meta: &fs::Metadata) -> bool {
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
pub(super) fn remove_stage(storage: &Storage, relative: &Path) -> Result<(), String> {
    let _parent_pin = storage.pin_directory(relative.parent().unwrap_or(Path::new("")), false)?;
    fn clean(storage: &Storage, relative: &Path) -> Result<(), String> {
        let pin = storage.pin_directory(relative, false)?;
        for item in fs::read_dir(storage.root.join(relative)).map_err(|_| "暂存目录无法清理。")?
        {
            let item = item.map_err(|_| "暂存目录无法清理。")?;
            let meta = fs::symlink_metadata(item.path()).map_err(|_| "暂存目录无法清理。")?;
            if linked(&meta) {
                return Err("暂存目录已变化，保留供人工检查。".into());
            }
            if meta.is_dir() {
                clean(storage, &relative.join(item.file_name()))?;
            } else {
                fs::remove_file(item.path()).map_err(|_| "暂存文件无法清理。")?;
            }
        }
        drop(pin);
        fs::remove_dir(storage.root.join(relative)).map_err(|_| "暂存目录无法清理。".into())
    }
    clean(storage, relative)
}

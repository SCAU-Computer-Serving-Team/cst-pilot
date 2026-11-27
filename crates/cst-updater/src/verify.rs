use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use cst_os::{Storage, is_reparse_or_symlink};
use regex::bytes::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use zip::ZipArchive;

use crate::io;
use crate::layout;
use crate::manifest::{MAX_FILES, ReleaseKey, VerifiedRelease};

pub struct VerifiedPackage {
    pub stage: Stage,
    pub release: VerifiedRelease,
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

    /// Extract a verified local archive inside `stage` and run full tree + credential verification.
    pub fn extract_and_verify(
        stage: Stage,
        archive: &Path,
        release: VerifiedRelease,
        cancel: &CancellationToken,
    ) -> Result<Self, String> {
        let _stage_pin = stage.base.pin_directory(&stage.relative, false)?;
        let local = Storage::new(&stage.root)?;
        let program = local.directory(Path::new("program"), true)?;
        extract_archive(archive, &program, &release, cancel)?;
        verify_tree(&program, &release, cancel)?;
        Ok(Self { stage, release })
    }

    /// Stage a local archive directly from bytes (offline verification / tests without network).
    pub fn stage_from_archive_bytes(
        root: &Path,
        release: VerifiedRelease,
        archive_bytes: &[u8],
        cancel: &CancellationToken,
    ) -> Result<Self, String> {
        stopped(cancel)?;
        let stage = Stage::create(root, &release)?;
        let archive = stage.archive_path();
        {
            let _pin = stage.base.pin_directory(&stage.relative, false)?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&archive)
                .map_err(|_| "无法创建升级下载文件。")?;
            file.write_all(archive_bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| "写入升级包失败。")?;
        }
        let actual_hash = format!("{:x}", Sha256::digest(archive_bytes));
        if archive_bytes.len() as u64 != release.info.archive.size
            || actual_hash != release.info.archive.sha256
        {
            return Err("升级包大小或 SHA-256 不匹配。".into());
        }
        stage.write_manifest(&release, cancel)?;
        Self::extract_and_verify(stage, &archive, release, cancel)
    }
}

pub struct Stage {
    pub root: PathBuf,
    pub retain: bool,
    pub base: Storage,
    pub relative: PathBuf,
}

impl Stage {
    pub fn create(root: &Path, release: &VerifiedRelease) -> Result<Self, String> {
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
        let relative = stage_path
            .strip_prefix(&storage.root)
            .map_err(|_| "暂存目录越界。")?
            .to_owned();
        Ok(Self {
            retain: false,
            relative,
            root: stage_path,
            base: storage,
        })
    }

    pub fn archive_path(&self) -> PathBuf {
        self.root.join("package.zip")
    }

    pub fn write_manifest(
        &self,
        release: &VerifiedRelease,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        let _pin = self.base.pin_directory(&self.relative, false)?;
        let local = Storage::new(&self.root)?;
        io::atomic(
            &local,
            &self.root.join("manifest.json"),
            release.envelope(),
            false,
            cancel,
        )
    }
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

pub fn extract_archive(
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
            layout::reject_retired_release_path(&relative)?;
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
        if layout::classify_release(&relative)? != layout::Area::Program {
            return Err("升级归档不能包含用户数据路径。".into());
        }
        let expected = release.sums.get(&relative);
        if expected.is_none() && relative != layout::SHA256SUMS_PATH {
            return Err("升级归档含未列入签名清单的文件。".into());
        }
        let limit = if relative == layout::SHA256SUMS_PATH {
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
        let path = storage.root.join(&relative);
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

/// Safely extracts an unsigned `cst-pilot-<ver>-windows-x64.zip` into `root` for offline signing
/// or validation, enforcing the same EOCD/ZIP64 bounds, path traversal/case/symlink/extra-field
/// guards, `Area::Program` classification, and total unpacked size cap as [`extract_archive`].
pub fn extract_clean_dist_archive(
    archive: &Path,
    root: &Path,
    max_unpacked_bytes: u64,
) -> Result<u64, String> {
    const MAX_SUMS_FILE_BYTES: u64 = 6 * 1024 * 1024;
    let archive_meta =
        std::fs::symlink_metadata(archive).map_err(|_| "发行归档不存在或无法读取。".to_owned())?;
    if cst_os::is_reparse_or_symlink(&archive_meta) || !archive_meta.is_file() {
        return Err("发行归档路径不能是符号链接、重解析点或目录。".into());
    }
    let archive_name = archive.file_name().ok_or("发行归档文件名无效。")?;
    let parent_dir = match archive.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let parent = Storage::new(parent_dir)?;
    let _parent_pin = parent.pin_directory(Path::new(""), false)?;
    let canonical_archive = parent.root.join(archive_name);
    let mut file = parent
        .open_read(&canonical_archive)?
        .ok_or("发行归档不存在。")?;
    zip_bounds(&mut file)?;
    let mut archive = ZipArchive::new(file).map_err(|_| "发行归档 ZIP 格式无效。")?;
    if archive.len() > MAX_FILES * 2 {
        return Err("发行归档条目过多。".into());
    }
    let storage = Storage::new(root)?;
    let mut seen = BTreeSet::new();
    let mut spellings = BTreeMap::new();
    let mut explicit_dirs = Vec::new();
    let mut extracted_files = Vec::new();
    let mut total_unpacked = 0u64;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|_| "发行归档条目无法读取。")?;
        let raw =
            std::str::from_utf8(entry.name_raw()).map_err(|_| "发行归档文件名必须是 UTF-8。")?;
        if raw != entry.name() {
            return Err("发行归档文件名编码存在歧义。".into());
        }
        let directory = entry.is_dir();
        let raw = if directory {
            raw.strip_suffix('/').unwrap_or(raw)
        } else {
            raw
        };
        let Some(relative) = raw.strip_prefix("cst-pilot/") else {
            if raw == "cst-pilot" && directory {
                continue;
            }
            return Err("发行归档必须仅包含 cst-pilot/ 顶层目录。".into());
        };
        layout::safe_path(relative)?;
        let relative = relative.to_owned();
        if !seen.insert(relative.to_lowercase()) {
            return Err("发行归档有重复或大小写冲突。".into());
        }
        check_spelling(&relative, &mut spellings)?;
        let mode = entry
            .unix_mode()
            .unwrap_or(if directory { 0o040000 } else { 0o100000 })
            & 0o170000;
        if entry.encrypted()
            || entry.is_symlink()
            || (mode != 0 && mode != if directory { 0o040000 } else { 0o100000 })
        {
            return Err("发行归档包含加密文件、链接或特殊文件。".into());
        }
        check_extra(entry.extra_data().unwrap_or_default())?;
        if directory {
            layout::reject_retired_release_path(&relative)?;
            if entry.size() != 0 {
                return Err("发行归档目录条目大小必须为 0。".into());
            }
            let _pin = storage.pin_directory(Path::new(&relative), true)?;
            explicit_dirs.push(relative);
            continue;
        }
        if layout::classify_release(&relative)? != layout::Area::Program {
            return Err("发行归档不能包含用户数据路径。".into());
        }
        let per_file_limit = if relative == layout::SHA256SUMS_PATH {
            MAX_SUMS_FILE_BYTES.min(max_unpacked_bytes)
        } else {
            max_unpacked_bytes
        };
        if entry.size() > per_file_limit {
            return Err("发行归档单文件体积超过限制。".into());
        }
        total_unpacked = total_unpacked
            .checked_add(entry.size())
            .filter(|n| *n <= max_unpacked_bytes)
            .ok_or("发行归档解压总体积超限。")?;
        let path = storage.root.join(&relative);
        let _pin =
            storage.pin_directory(Path::new(&relative).parent().unwrap_or(Path::new("")), true)?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|_| "无法创建发行解包文件。")?;
        let mut copied = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let n = entry
                .read(&mut buffer)
                .map_err(|_| "发行归档解压或 CRC 校验失败。")?;
            if n == 0 {
                break;
            }
            copied = copied
                .checked_add(n as u64)
                .filter(|v| *v <= per_file_limit)
                .ok_or("发行归档文件实际解压体积超限。")?;
            output
                .write_all(&buffer[..n])
                .map_err(|_| "发行解包写入失败。")?;
        }
        if copied != entry.size() {
            return Err("发行归档文件解压大小与声明不一致。".into());
        }
        output.sync_all().map_err(|_| "发行解包同步失败。")?;
        extracted_files.push(relative);
    }

    for dir in &explicit_dirs {
        let prefix = format!("{dir}/");
        if !extracted_files.iter().any(|f| f.starts_with(&prefix)) {
            return Err(format!("发行归档包含空目录或未受管目录条目：{dir}"));
        }
    }

    Ok(total_unpacked)
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

pub fn verify_tree(
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
            if is_reparse_or_symlink(&meta) {
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
                if layout::classify_release(&name)? != layout::Area::Program {
                    return Err("升级目录不能包含用户数据路径。".into());
                }
                if !seen.insert(name.clone()) {
                    return Err("升级目录文件重复。".into());
                }
                let mut file = storage.open_read(&item.path())?.ok_or("升级文件消失。")?;
                if name == layout::SHA256SUMS_PATH {
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
                        let value = crate::json::parse(&bytes)?;
                        check_json(&value)?;
                    }
                }
            }
        }
    }
    if seen.len() != release.sums.len() + 1
        || !seen.contains(layout::SHA256SUMS_PATH)
        || !release.sums.keys().all(|p| seen.contains(p))
    {
        return Err("升级包缺少签名清单中的文件。".into());
    }
    layout::validate_metadata(&release.sums, &storage.root, &release.info.version)
}

/// Scans an assembled release directory using the exact same whitelist, redline,
/// credential, JSON, and SFNT checks enforced by the online updater, returning
/// relative path -> lowercase SHA-256 hex (excluding `app/SHA256SUMS`).
pub fn scan_clean_dist_tree(
    root: &Path,
    target_version: &str,
) -> Result<BTreeMap<String, String>, String> {
    let storage = Storage::new(root)?;
    let mut sums = BTreeMap::new();
    let mut seen_lower = BTreeSet::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let _pin = storage.pin_directory(&relative, false)?;
        for item in fs::read_dir(storage.root.join(&relative)).map_err(|_| "无法扫描发行目录。")?
        {
            let item = item.map_err(|_| "无法扫描发行目录。")?;
            let rel = relative.join(item.file_name());
            let name = rel.to_str().ok_or("发行文件名无效。")?.replace('\\', "/");
            let meta = fs::symlink_metadata(item.path()).map_err(|_| "无法检查发行文件。")?;
            if is_reparse_or_symlink(&meta) {
                return Err(format!("发行目录包含链接或重解析点：{name}"));
            }
            if meta.is_dir() {
                layout::safe_path(&name)?;
                layout::reject_retired_release_path(&name)?;
                pending.push(rel);
                continue;
            }
            if name == layout::SHA256SUMS_PATH {
                continue;
            }
            if layout::classify_release(&name)? != layout::Area::Program {
                return Err(format!("发行目录包含非程序区文件：{name}"));
            }
            if !seen_lower.insert(name.to_lowercase()) {
                return Err(format!("发行目录存在大小写冲突文件：{name}"));
            }
            let mut file = storage.open_read(&item.path())?.ok_or("发行文件消失。")?;
            let mut hash = Sha256::new();
            let mut buffer = [0; 64 * 1024];
            let mut secret_scan = SecretScan::default();
            loop {
                let n = file.read(&mut buffer).map_err(|_| "无法读取发行文件。")?;
                if n == 0 {
                    break;
                }
                hash.update(&buffer[..n]);
                secret_scan.check(&buffer[..n])?;
            }
            if name.ends_with(".json") {
                let bytes = storage
                    .read_limited(&item.path(), 8 * 1024 * 1024)?
                    .ok_or("发行 JSON 文件消失。")?;
                let value = crate::json::parse(&bytes)?;
                check_json(&value)?;
            }
            sums.insert(name, format!("{:x}", hash.finalize()));
        }
    }
    layout::validate_metadata(&sums, &storage.root, target_version)?;
    Ok(sums)
}

/// Checks the assembled file set and contents against its on-disk checksum table.
pub fn verify_clean_dist_tree(root: &Path, version: &str) -> Result<usize, String> {
    let actual = scan_clean_dist_tree(root, version)?;
    let storage = Storage::new(root)?;
    let bytes = storage
        .read_limited(&storage.root.join(layout::SHA256SUMS_PATH), 4 * 1024 * 1024)?
        .ok_or("发行目录缺少 SHA256SUMS。")?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "SHA256SUMS 编码无效。")?;
    check_dist_sums(&actual, text)?;
    Ok(actual.len())
}

fn check_dist_sums(actual: &BTreeMap<String, String>, text: &str) -> Result<(), String> {
    let expected = crate::manifest::parse_sums(text)?;
    if let Some((path, _)) = actual
        .iter()
        .find(|(path, hash)| expected.get(*path) != Some(*hash))
    {
        return Err(format!("发行文件未登记或 SHA-256 不匹配：{path}"));
    }
    if let Some(path) = expected.keys().find(|path| !actual.contains_key(*path)) {
        return Err(format!("SHA256SUMS 登记了不存在的发行文件：{path}"));
    }
    Ok(())
}

#[cfg(test)]
mod dist_sum_tests {
    use super::*;

    #[test]
    fn rejects_tampering_missing_and_extra_files() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let actual = BTreeMap::from([("VERSION".to_owned(), a.clone())]);
        let good = format!("{a}  VERSION\n");
        assert!(check_dist_sums(&actual, &good).is_ok());
        assert!(check_dist_sums(&actual, &format!("{b}  VERSION\n")).is_err());
        assert!(check_dist_sums(&actual, &format!("{a}  cst-pilot.exe\n")).is_err());
        assert!(check_dist_sums(&actual, &format!("{good}{b}  cst-pilot.exe\n")).is_err());
    }
}

#[derive(Default)]
struct SecretScan {
    tail: Vec<u8>,
}

impl SecretScan {
    fn check(&mut self, bytes: &[u8]) -> Result<(), String> {
        static PATTERN: OnceLock<Regex> = OnceLock::new();
        let pattern = PATTERN.get_or_init(|| {
            Regex::new(r"(?-u:\b(?:sk-(?:proj-|ant-api[0-9]+-)?[A-Za-z0-9_-]{20,}|gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,}|AIza[0-9A-Za-z_-]{30,}|AKIA[0-9A-Z]{16})|eyJ[A-Za-z0-9_-]{15,}\.[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}|-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----)")
                .expect("fixed credential pattern")
        });
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

pub fn remove_stage(storage: &Storage, relative: &Path) -> Result<(), String> {
    let _parent_pin = storage.pin_directory(relative.parent().unwrap_or(Path::new("")), false)?;
    fn clean(storage: &Storage, relative: &Path) -> Result<(), String> {
        let pin = storage.pin_directory(relative, false)?;
        for item in fs::read_dir(storage.root.join(relative)).map_err(|_| "暂存目录无法清理。")?
        {
            let item = item.map_err(|_| "暂存目录无法清理。")?;
            let meta = fs::symlink_metadata(item.path()).map_err(|_| "暂存目录无法清理。")?;
            if is_reparse_or_symlink(&meta) {
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

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
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
        let actual_hash = format!("{:x}", Sha256::digest(archive_bytes));
        if archive_bytes.len() as u64 != release.info.archive.size
            || actual_hash != release.info.archive.sha256
        {
            return Err("升级包大小或 SHA-256 不匹配。".into());
        }
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
        stage.write_manifest(&release, cancel)?;
        {
            let _stage_pin = stage.base.pin_directory(&stage.relative, false)?;
            let local = Storage::new(&stage.root)?;
            let program = local.directory(Path::new("program"), true)?;
            extract_archive_bytes(archive_bytes, &program, &release, cancel)?;
            verify_tree(&program, &release, cancel)?;
        }
        Ok(Self { stage, release })
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

fn open_pinned_target_storage(
    root: &Path,
    label: &str,
) -> Result<(Storage, cst_os::DirectoryLease, Storage), String> {
    let root_meta = fs::symlink_metadata(root).map_err(|_| format!("{label}不存在或无法读取。"))?;
    if is_reparse_or_symlink(&root_meta) || !root_meta.is_dir() {
        return Err(format!("{label}不能是符号链接、重解析点或普通文件。"));
    }
    let root_name = root
        .file_name()
        .ok_or_else(|| format!("{label}路径无效。"))?;
    let parent_dir = match root.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let parent_storage = Storage::new(parent_dir)?;
    let lease = parent_storage.pin_directory(Path::new(root_name), false)?;
    let pinned_target = parent_storage.root.join(root_name);
    let storage = Storage::new(&pinned_target)?;
    if !lease.leaf_same_handle(&storage.root)? {
        return Err(format!("{label}真实身份与已锁定句柄不一致。"));
    }
    Ok((parent_storage, lease, storage))
}

/// Validates EOCD/ZIP64 central-directory bounds, entry count caps, and 1-to-1 Central Directory
/// vs Local File Header consistency on an in-memory ZIP slice before constructing `ZipArchive`.
pub fn validate_zip_bounds_bytes(zip_bytes: &[u8]) -> Result<(), String> {
    let mut cursor = std::io::Cursor::new(zip_bytes);
    zip_bounds_reader(&mut cursor, zip_bytes.len() as u64).map(|_| ())
}

fn verify_empty_zip_dir_entry<R: Read>(
    entry: &mut zip::read::ZipFile<'_, R>,
) -> Result<(), String> {
    if entry.size() != 0 || entry.compressed_size() != 0 {
        return Err("归档目录条目不允许携带压缩数据或非零大小。".into());
    }
    let mut probe = [0u8; 1];
    let n = entry
        .read(&mut probe)
        .map_err(|_| "归档目录条目读取校验失败。")?;
    if n != 0 {
        return Err("归档目录条目实际解压内容不为空。".into());
    }
    Ok(())
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
    let len = file.metadata().map_err(|_| "升级包无法读取元数据。")?.len();
    extract_archive_reader(&mut file, len, root, release, cancel)
}

pub fn extract_archive_bytes(
    zip_bytes: &[u8],
    root: &Path,
    release: &VerifiedRelease,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let mut cursor = std::io::Cursor::new(zip_bytes);
    extract_archive_reader(&mut cursor, zip_bytes.len() as u64, root, release, cancel)
}

fn extract_archive_reader<R: Read + Seek>(
    reader: &mut R,
    reader_len: u64,
    root: &Path,
    release: &VerifiedRelease,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let records = zip_bounds_reader(reader, reader_len)?;
    let mut archive = ZipArchive::new(reader).map_err(|_| "升级包 ZIP 格式无效。")?;
    if archive.len() != records.len() || archive.len() > MAX_FILES * 2 {
        return Err("升级归档条目数量异常或与中央目录不一致。".into());
    }
    let (_parent_storage, root_lease, storage) = open_pinned_target_storage(root, "升级解包目录")?;
    let mut seen = BTreeSet::new();
    let mut spellings = BTreeMap::new();
    let mut saw_root_dir = false;
    let mut count = 0u64;
    let mut written_total = 0u64;
    let sums_size = release.info.sha256_sums.len() as u64;
    for (i, expected_rec) in records.iter().enumerate() {
        stopped(cancel)?;
        let mut file = archive.by_index(i).map_err(|_| "升级归档条目无法读取。")?;
        if file.name_raw() != expected_rec.name.as_slice()
            || file.central_header_start() != expected_rec.central_header_start
            || file.header_start() != expected_rec.local_header_start
            || file.compressed_size() != expected_rec.compressed_size
            || file.size() != expected_rec.uncompressed_size
        {
            return Err("升级归档解析结果与中央目录记录不一致。".into());
        }
        let raw =
            std::str::from_utf8(file.name_raw()).map_err(|_| "升级归档文件名必须是 UTF-8。")?;
        if raw != file.name() {
            return Err("升级归档文件名编码存在歧义。".into());
        }
        let directory = file.is_dir();
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
        check_parsed_zipfile_extra(file.extra_data().unwrap_or_default())?;
        let raw = if directory {
            raw.strip_suffix('/').unwrap_or(raw)
        } else {
            raw
        };
        let Some(relative) = raw.strip_prefix("cst-pilot/") else {
            if raw == "cst-pilot" && directory {
                if saw_root_dir {
                    return Err("升级归档顶层目录条目异常或重复。".into());
                }
                verify_empty_zip_dir_entry(&mut file)?;
                saw_root_dir = true;
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
        if directory {
            layout::validate_release_dir_path(&relative)?;
            verify_empty_zip_dir_entry(&mut file)?;
            if !release
                .sums
                .keys()
                .any(|p| p.starts_with(&format!("{relative}/")))
            {
                return Err("升级归档目录不在签名清单中。".into());
            }
            let _pin = storage.pin_directory_from_lease(&root_lease, Path::new(&relative), true)?;
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
        let entry_size = file.size();
        if entry_size > limit {
            return Err("升级文件体积超过限制。".into());
        }
        count = count
            .checked_add(entry_size)
            .filter(|n| *n <= release.info.archive.unpacked_size)
            .ok_or("升级解压总体积超限。")?;
        let rel_path = Path::new(&relative);
        let file_name = rel_path.file_name().ok_or("升级归档文件名无效。")?;
        let parent_rel = rel_path.parent().unwrap_or(Path::new(""));
        let parent_dir = storage.root.join(parent_rel);
        let pin = storage.pin_directory_from_lease(&root_lease, parent_rel, true)?;
        let mut output = pin
            .create_new_file(&parent_dir, file_name)
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
                .filter(|n| *n <= entry_size)
                .ok_or("升级文件解压体积超限。")?;
            written_total = written_total
                .checked_add(n as u64)
                .filter(|n| *n <= release.info.archive.unpacked_size)
                .ok_or("升级文件累计解压体积超限。")?;
            hash.update(&buffer[..n]);
            output
                .write_all(&buffer[..n])
                .map_err(|_| "升级文件写入失败。")?;
        }
        if read != entry_size || expected.is_some_and(|v| *v != format!("{:x}", hash.finalize())) {
            return Err("升级文件哈希或体积不匹配。".into());
        }
        output.sync_all().map_err(|_| "升级文件同步失败。")?;
    }
    if count != release.info.archive.unpacked_size || written_total != count {
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
    let len = file
        .metadata()
        .map_err(|_| "发行归档无法读取元数据。")?
        .len();
    extract_clean_dist_archive_reader(&mut file, len, root, max_unpacked_bytes)
}

/// Extracts directly from an already hash-verified in-memory `zip_bytes` slice without writing
/// or re-opening any temporary archive file on disk.
pub fn extract_clean_dist_archive_bytes(
    zip_bytes: &[u8],
    root: &Path,
    max_unpacked_bytes: u64,
) -> Result<u64, String> {
    let mut cursor = std::io::Cursor::new(zip_bytes);
    extract_clean_dist_archive_reader(
        &mut cursor,
        zip_bytes.len() as u64,
        root,
        max_unpacked_bytes,
    )
}

fn extract_clean_dist_archive_reader<R: Read + Seek>(
    reader: &mut R,
    reader_len: u64,
    root: &Path,
    max_unpacked_bytes: u64,
) -> Result<u64, String> {
    const MAX_SUMS_FILE_BYTES: u64 = 6 * 1024 * 1024;
    let records = zip_bounds_reader(reader, reader_len)?;
    let mut archive = ZipArchive::new(reader).map_err(|_| "发行归档 ZIP 格式无效。")?;
    if archive.len() != records.len() || archive.len() > MAX_FILES * 2 {
        return Err("发行归档条目数量异常或与中央目录不一致。".into());
    }
    let (_parent_storage, root_lease, storage) = open_pinned_target_storage(root, "解包目标目录")?;
    let mut seen = BTreeSet::new();
    let mut spellings = BTreeMap::new();
    let mut saw_root_dir = false;
    let mut explicit_dirs = Vec::new();
    let mut extracted_files = Vec::new();
    let mut total_unpacked = 0u64;
    let mut written_total = 0u64;

    for (i, expected_rec) in records.iter().enumerate() {
        let mut entry = archive.by_index(i).map_err(|_| "发行归档条目无法读取。")?;
        if entry.name_raw() != expected_rec.name.as_slice()
            || entry.central_header_start() != expected_rec.central_header_start
            || entry.header_start() != expected_rec.local_header_start
            || entry.compressed_size() != expected_rec.compressed_size
            || entry.size() != expected_rec.uncompressed_size
        {
            return Err("发行归档解析结果与中央目录记录不一致。".into());
        }
        let raw =
            std::str::from_utf8(entry.name_raw()).map_err(|_| "发行归档文件名必须是 UTF-8。")?;
        if raw != entry.name() {
            return Err("发行归档文件名编码存在歧义。".into());
        }
        let directory = entry.is_dir();
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
        check_parsed_zipfile_extra(entry.extra_data().unwrap_or_default())?;
        let raw = if directory {
            raw.strip_suffix('/').unwrap_or(raw)
        } else {
            raw
        };
        let Some(relative) = raw.strip_prefix("cst-pilot/") else {
            if raw == "cst-pilot" && directory {
                if saw_root_dir {
                    return Err("发行归档顶层目录条目异常或重复。".into());
                }
                verify_empty_zip_dir_entry(&mut entry)?;
                saw_root_dir = true;
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
        if directory {
            layout::validate_release_dir_path(&relative)?;
            verify_empty_zip_dir_entry(&mut entry)?;
            let _pin = storage.pin_directory_from_lease(&root_lease, Path::new(&relative), true)?;
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
        let entry_size = entry.size();
        if entry_size > per_file_limit {
            return Err("发行归档单文件体积超过限制。".into());
        }
        total_unpacked = total_unpacked
            .checked_add(entry_size)
            .filter(|n| *n <= max_unpacked_bytes)
            .ok_or("发行归档解压总体积超限。")?;
        let rel_path = Path::new(&relative);
        let file_name = rel_path.file_name().ok_or("发行归档文件名无效。")?;
        let parent_rel = rel_path.parent().unwrap_or(Path::new(""));
        let parent_dir = storage.root.join(parent_rel);
        let pin = storage.pin_directory_from_lease(&root_lease, parent_rel, true)?;
        let mut output = pin
            .create_new_file(&parent_dir, file_name)
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
                .filter(|v| *v <= entry_size)
                .ok_or("发行归档文件实际解压体积超限。")?;
            written_total = written_total
                .checked_add(n as u64)
                .filter(|v| *v <= max_unpacked_bytes)
                .ok_or("发行归档实际写入总体积超限。")?;
            output
                .write_all(&buffer[..n])
                .map_err(|_| "发行解包写入失败。")?;
        }
        if copied != entry_size {
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct VerifiedZipRecord {
    name: Vec<u8>,
    central_header_start: u64,
    local_header_start: u64,
    compressed_size: u64,
    uncompressed_size: u64,
}

#[cfg(test)]
fn zip_bounds(file: &mut std::fs::File) -> Result<Vec<VerifiedZipRecord>, String> {
    let len = file
        .metadata()
        .map_err(|_| "升级 ZIP 元数据无法读取。")?
        .len();
    zip_bounds_reader(file, len)
}

fn zip_bounds_reader<R: Read + Seek>(
    file: &mut R,
    len: u64,
) -> Result<Vec<VerifiedZipRecord>, String> {
    let error = || "升级 ZIP 的目录结构无效或超过资源限制。".to_owned();
    let start = len.saturating_sub(65_557);
    file.seek(SeekFrom::Start(start)).map_err(|_| error())?;
    let mut tail = Vec::new();
    file.by_ref()
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
    let eocd_count = u64::from(u16::from_le_bytes(
        eocd[10..12].try_into().map_err(|_| error())?,
    ));
    let eocd_size = u64::from(u32::from_le_bytes(
        eocd[12..16].try_into().map_err(|_| error())?,
    ));
    let eocd_offset = u64::from(u32::from_le_bytes(
        eocd[16..20].try_into().map_err(|_| error())?,
    ));
    if eocd_size == 0xffff_ffff && eocd_count != 0xffff && eocd_offset != 0xffff_ffff {
        return Err(error());
    }
    let mut count = eocd_count;
    let mut size = eocd_size;
    let mut offset = eocd_offset;
    let cd_limit = if eocd_count == 0xffff || eocd_offset == 0xffff_ffff {
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
        let z_count = u64::from_le_bytes(zip64[32..40].try_into().map_err(|_| error())?);
        let z_size = u64::from_le_bytes(zip64[40..48].try_into().map_err(|_| error())?);
        let z_offset = u64::from_le_bytes(zip64[48..56].try_into().map_err(|_| error())?);
        if (eocd_count != 0xffff && z_count != eocd_count)
            || (eocd_size != 0xffff_ffff && z_size != eocd_size)
            || (eocd_offset != 0xffff_ffff && z_offset != eocd_offset)
        {
            return Err(error());
        }
        count = z_count;
        size = z_size;
        offset = z_offset;
        pos
    } else {
        start + at as u64
    };
    if count == 0
        || count > (MAX_FILES * 2) as u64
        || size > 64 * 1024 * 1024
        || offset.checked_add(size) != Some(cd_limit)
    {
        return Err(error());
    }
    let records = verify_central_and_local_headers(file, count as usize, offset, size as usize)?;
    file.seek(SeekFrom::Start(0)).map_err(|_| error())?;
    Ok(records)
}

fn verify_central_and_local_headers<R: Read + Seek>(
    file: &mut R,
    expected_count: usize,
    cd_offset: u64,
    cd_size: usize,
) -> Result<Vec<VerifiedZipRecord>, String> {
    let error = || "升级 ZIP 中央目录与本地文件头不一致或存在重复条目。".to_owned();
    file.seek(SeekFrom::Start(cd_offset)).map_err(|_| error())?;
    let mut cd = vec![0u8; cd_size];
    file.read_exact(&mut cd).map_err(|_| error())?;

    let mut pos = 0usize;
    let mut seen_names = BTreeSet::new();
    let mut records = Vec::with_capacity(expected_count);
    let mut spans = Vec::with_capacity(expected_count);
    while pos < cd.len() {
        if records.len() >= expected_count
            || pos + 46 > cd.len()
            || &cd[pos..pos + 4] != b"PK\x01\x02"
        {
            return Err(error());
        }
        let cd_flags = u16::from_le_bytes([cd[pos + 8], cd[pos + 9]]);
        let cd_method = u16::from_le_bytes([cd[pos + 10], cd[pos + 11]]);
        let cd_mtime_mdate = &cd[pos + 12..pos + 16];
        let cd_crc32 = u32::from_le_bytes([cd[pos + 16], cd[pos + 17], cd[pos + 18], cd[pos + 19]]);
        let comp_32 = u32::from_le_bytes([cd[pos + 20], cd[pos + 21], cd[pos + 22], cd[pos + 23]]);
        let uncomp_32 =
            u32::from_le_bytes([cd[pos + 24], cd[pos + 25], cd[pos + 26], cd[pos + 27]]);
        let name_len = u16::from_le_bytes([cd[pos + 28], cd[pos + 29]]) as usize;
        let extra_len = u16::from_le_bytes([cd[pos + 30], cd[pos + 31]]) as usize;
        let comment_len = u16::from_le_bytes([cd[pos + 32], cd[pos + 33]]) as usize;
        let disk_16 = u16::from_le_bytes([cd[pos + 34], cd[pos + 35]]);
        let ext_attr = u32::from_le_bytes([cd[pos + 38], cd[pos + 39], cd[pos + 40], cd[pos + 41]]);
        let lfh_32 = u32::from_le_bytes([cd[pos + 42], cd[pos + 43], cd[pos + 44], cd[pos + 45]]);

        // Reject encrypted bit (0x01), data descriptor bit (0x08), or multi-disk archive entries.
        if (cd_flags & 0x0009) != 0 || disk_16 != 0 {
            return Err(error());
        }
        let record_end = pos
            .checked_add(46)
            .and_then(|v| v.checked_add(name_len))
            .and_then(|v| v.checked_add(extra_len))
            .and_then(|v| v.checked_add(comment_len))
            .ok_or_else(error)?;
        if record_end > cd.len() || name_len == 0 {
            return Err(error());
        }
        let cd_name_bytes = &cd[pos + 46..pos + 46 + name_len];
        let cd_name = std::str::from_utf8(cd_name_bytes).map_err(|_| error())?;
        if cd_name.contains('\\') {
            return Err(error());
        }
        let unix_ftype = (ext_attr >> 16) & 0o170000;
        let is_dir_name = cd_name.ends_with('/');
        if (!is_dir_name && ((ext_attr & 0x10) != 0 || unix_ftype == 0o040000))
            || (is_dir_name && unix_ftype != 0 && unix_ftype != 0o040000)
        {
            return Err(error());
        }
        let normalized_key = cd_name.trim_end_matches('/').to_lowercase();
        if !seen_names.insert(normalized_key) {
            return Err("升级 ZIP 中央目录存在重复或大小写冲突条目。".into());
        }
        let cd_extra = &cd[pos + 46 + name_len..pos + 46 + name_len + extra_len];

        let need_uncomp = uncomp_32 == 0xffff_ffff;
        let need_comp = comp_32 == 0xffff_ffff;
        let need_off = lfh_32 == 0xffff_ffff;
        let expected_cd_zip64_len =
            (usize::from(need_uncomp) + usize::from(need_comp) + usize::from(need_off)) * 8;
        let cd_parsed_extra = check_extra(cd_extra, expected_cd_zip64_len, true)?;

        let (uncompressed_size, compressed_size, local_offset) = if expected_cd_zip64_len > 0 {
            let body = cd_parsed_extra.zip64.ok_or_else(error)?;
            let mut zoff = 0usize;
            let uncomp = if need_uncomp {
                let v = u64::from_le_bytes(body[zoff..zoff + 8].try_into().map_err(|_| error())?);
                zoff += 8;
                v
            } else {
                u64::from(uncomp_32)
            };
            let comp = if need_comp {
                let v = u64::from_le_bytes(body[zoff..zoff + 8].try_into().map_err(|_| error())?);
                zoff += 8;
                v
            } else {
                u64::from(comp_32)
            };
            let loff = if need_off {
                u64::from_le_bytes(body[zoff..zoff + 8].try_into().map_err(|_| error())?)
            } else {
                u64::from(lfh_32)
            };
            (uncomp, comp, loff)
        } else {
            (u64::from(uncomp_32), u64::from(comp_32), u64::from(lfh_32))
        };

        if cd_name.ends_with('/')
            && (uncompressed_size != 0 || compressed_size != 0 || cd_method != 0 || cd_crc32 != 0)
        {
            return Err("升级 ZIP 目录条目不允许携带压缩数据或非零大小。".into());
        }

        if local_offset
            .checked_add(30)
            .is_none_or(|end| end > cd_offset)
        {
            return Err(error());
        }
        file.seek(SeekFrom::Start(local_offset))
            .map_err(|_| error())?;
        let mut lfh = [0u8; 30];
        file.read_exact(&mut lfh).map_err(|_| error())?;
        if &lfh[..4] != b"PK\x03\x04" {
            return Err(error());
        }
        let lfh_flags = u16::from_le_bytes([lfh[6], lfh[7]]);
        let lfh_method = u16::from_le_bytes([lfh[8], lfh[9]]);
        let lfh_mtime_mdate = &lfh[10..14];
        let lfh_crc32 = u32::from_le_bytes([lfh[14], lfh[15], lfh[16], lfh[17]]);
        let lfh_comp32 = u32::from_le_bytes([lfh[18], lfh[19], lfh[20], lfh[21]]);
        let lfh_uncomp32 = u32::from_le_bytes([lfh[22], lfh[23], lfh[24], lfh[25]]);
        let lfh_name_len = u16::from_le_bytes([lfh[26], lfh[27]]) as usize;
        let lfh_extra_len = u16::from_le_bytes([lfh[28], lfh[29]]) as usize;
        if lfh_flags != cd_flags
            || lfh_method != cd_method
            || lfh_mtime_mdate != cd_mtime_mdate
            || lfh_crc32 != cd_crc32
            || lfh_name_len != name_len
        {
            return Err(error());
        }
        let var_end = local_offset
            .checked_add(30)
            .and_then(|v| v.checked_add(lfh_name_len as u64))
            .and_then(|v| v.checked_add(lfh_extra_len as u64))
            .ok_or_else(error)?;
        let data_end = var_end.checked_add(compressed_size).ok_or_else(error)?;
        if data_end > cd_offset {
            return Err(error());
        }
        let mut lfh_var = vec![0u8; lfh_name_len + lfh_extra_len];
        file.read_exact(&mut lfh_var).map_err(|_| error())?;
        if &lfh_var[..lfh_name_len] != cd_name_bytes {
            return Err(error());
        }
        let lfh_need_zip64 = lfh_uncomp32 == 0xffff_ffff || lfh_comp32 == 0xffff_ffff;
        let lfh_zip64_len = if lfh_need_zip64 { 16 } else { 0 };
        let lfh_parsed_extra = check_extra(&lfh_var[lfh_name_len..], lfh_zip64_len, false)?;
        if lfh_need_zip64 {
            if lfh_uncomp32 != 0xffff_ffff || lfh_comp32 != 0xffff_ffff {
                return Err(error());
            }
            let lfh_zip64 = lfh_parsed_extra.zip64.ok_or_else(error)?;
            let lfh_u64 = u64::from_le_bytes(lfh_zip64[0..8].try_into().map_err(|_| error())?);
            let lfh_c64 = u64::from_le_bytes(lfh_zip64[8..16].try_into().map_err(|_| error())?);
            if lfh_u64 != uncompressed_size || lfh_c64 != compressed_size {
                return Err(error());
            }
        } else if u64::from(lfh_uncomp32) != uncompressed_size
            || u64::from(lfh_comp32) != compressed_size
        {
            return Err(error());
        }
        if let (Some((cd_ts_flags, cd_mtime)), Some((lfh_ts_flags, lfh_mtime))) =
            (cd_parsed_extra.ext_ts, lfh_parsed_extra.ext_ts)
            && (cd_ts_flags != lfh_ts_flags || cd_mtime != lfh_mtime)
        {
            return Err("升级 ZIP 中央目录与本地头时间戳扩展字段不一致。".into());
        }
        if let (Some(cd_ug), Some(lfh_ug)) = (cd_parsed_extra.unix_ug, lfh_parsed_extra.unix_ug)
            && cd_ug != lfh_ug
        {
            return Err("升级 ZIP 中央目录与本地头 Unix UID/GID 扩展字段不一致。".into());
        }
        if let (Some(cd_ntfs), Some(lfh_ntfs)) =
            (cd_parsed_extra.ntfs_times, lfh_parsed_extra.ntfs_times)
            && cd_ntfs != lfh_ntfs
        {
            return Err("升级 ZIP 中央目录与本地头 NTFS 扩展字段不一致。".into());
        }

        spans.push((local_offset, data_end));
        records.push(VerifiedZipRecord {
            name: cd_name_bytes.to_vec(),
            central_header_start: cd_offset + pos as u64,
            local_header_start: local_offset,
            compressed_size,
            uncompressed_size,
        });
        pos = record_end;
    }
    if records.len() != expected_count {
        return Err(error());
    }
    spans.sort_unstable_by_key(|&(start, _)| start);
    let mut cursor = 0u64;
    for (start, end) in spans {
        if start != cursor {
            return Err(error());
        }
        cursor = end;
    }
    if cursor != cd_offset {
        return Err(error());
    }
    Ok(records)
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

#[derive(Default)]
struct ParsedExtra<'a> {
    zip64: Option<&'a [u8]>,
    ext_ts: Option<(u8, Option<&'a [u8]>)>,
    unix_ug: Option<&'a [u8]>,
    ntfs_times: Option<&'a [u8]>,
}

fn check_extra(
    mut extra: &[u8],
    expected_zip64_len: usize,
    is_central: bool,
) -> Result<ParsedExtra<'_>, String> {
    let mut out = ParsedExtra::default();
    while !extra.is_empty() {
        if extra.len() < 4 {
            return Err("升级 ZIP 扩展字段无效。".into());
        }
        let id = u16::from_le_bytes([extra[0], extra[1]]);
        let len = u16::from_le_bytes([extra[2], extra[3]]) as usize;
        if !matches!(id, 0x0001 | 0x5455 | 0x7875 | 0x000a) || len > extra.len() - 4 {
            return Err("升级 ZIP 包含未支持的链接或扩展元数据。".into());
        }
        let body = &extra[4..4 + len];
        match id {
            0x0001 => {
                if expected_zip64_len == 0 || len != expected_zip64_len || out.zip64.is_some() {
                    return Err("升级 ZIP 包含异常或冗余的 ZIP64 扩展字段。".into());
                }
                out.zip64 = Some(body);
            }
            0x5455 => {
                if body.is_empty() || out.ext_ts.is_some() {
                    return Err("升级 ZIP 时间戳扩展字段缺失或重复。".into());
                }
                let flags = body[0];
                if (flags & !0x07) != 0 {
                    return Err("升级 ZIP 时间戳扩展字段标志位无效。".into());
                }
                let expected_len = if is_central {
                    1 + usize::from((flags & 0x01) != 0) * 4
                } else {
                    1 + (flags.count_ones() as usize) * 4
                };
                if body.len() != expected_len {
                    return Err("升级 ZIP 时间戳扩展字段长度与标志位不匹配。".into());
                }
                let mtime = ((flags & 0x01) != 0).then(|| &body[1..5]);
                out.ext_ts = Some((flags, mtime));
            }
            0x7875 => {
                if body.len() < 3 || body[0] != 1 || out.unix_ug.is_some() {
                    return Err("升级 ZIP Unix UID/GID 扩展字段格式无效或重复。".into());
                }
                let uid_len = body[1] as usize;
                if 2 + uid_len >= body.len() {
                    return Err("升级 ZIP Unix UID 扩展字段越界。".into());
                }
                let gid_len = body[2 + uid_len] as usize;
                if 3 + uid_len + gid_len != body.len() {
                    return Err("升级 ZIP Unix GID 扩展字段长度不匹配。".into());
                }
                out.unix_ug = Some(body);
            }
            0x000a => {
                if body.len() != 32 || body[0..4] != [0, 0, 0, 0] || out.ntfs_times.is_some() {
                    return Err("升级 ZIP NTFS 扩展字段头或固定长度无效。".into());
                }
                let sub_tag = u16::from_le_bytes([body[4], body[5]]);
                let sub_len = u16::from_le_bytes([body[6], body[7]]) as usize;
                if sub_tag != 0x0001 || sub_len != 24 {
                    return Err("升级 ZIP NTFS 子属性类型或长度无效。".into());
                }
                out.ntfs_times = Some(&body[8..32]);
            }
            _ => unreachable!(),
        }
        extra = &extra[4 + len..];
    }
    if expected_zip64_len > 0 && out.zip64.is_none() {
        return Err("升级 ZIP 缺少必需的 ZIP64 扩展字段。".into());
    }
    Ok(out)
}

/// Validates `ZipFile::extra_data()` returned by `zip 6.0.0` (which strips `0x0001` and may
/// concatenate Central Directory and Local File Header extra blocks, where CD `0x5455` carries
/// only `mtime` even if `atime`/`ctime` flag bits are set).
fn check_parsed_zipfile_extra(mut extra: &[u8]) -> Result<(), String> {
    while !extra.is_empty() {
        if extra.len() < 4 {
            return Err("升级 ZIP 扩展字段无效。".into());
        }
        let id = u16::from_le_bytes([extra[0], extra[1]]);
        let len = u16::from_le_bytes([extra[2], extra[3]]) as usize;
        if !matches!(id, 0x5455 | 0x7875 | 0x000a) || len > extra.len() - 4 {
            return Err("升级 ZIP 包含未支持的链接或扩展元数据。".into());
        }
        let body = &extra[4..4 + len];
        match id {
            0x5455 => {
                if body.is_empty() {
                    return Err("升级 ZIP 时间戳扩展字段缺失。".into());
                }
                let flags = body[0];
                if (flags & !0x07) != 0 {
                    return Err("升级 ZIP 时间戳扩展字段标志位无效。".into());
                }
                let cd_len = 1 + usize::from((flags & 0x01) != 0) * 4;
                let lfh_len = 1 + (flags.count_ones() as usize) * 4;
                if body.len() != cd_len && body.len() != lfh_len {
                    return Err("升级 ZIP 时间戳扩展字段长度无效。".into());
                }
            }
            0x7875 => {
                if body.len() < 3 || body[0] != 1 {
                    return Err("升级 ZIP Unix UID/GID 扩展字段格式无效。".into());
                }
                let uid_len = body[1] as usize;
                if 2 + uid_len >= body.len() {
                    return Err("升级 ZIP Unix UID 扩展字段越界。".into());
                }
                let gid_len = body[2 + uid_len] as usize;
                if 3 + uid_len + gid_len != body.len() {
                    return Err("升级 ZIP Unix GID 扩展字段长度不匹配。".into());
                }
            }
            0x000a => {
                if body.len() != 32 || body[0..4] != [0, 0, 0, 0] {
                    return Err("升级 ZIP NTFS 扩展字段头或固定长度无效。".into());
                }
                let sub_tag = u16::from_le_bytes([body[4], body[5]]);
                let sub_len = u16::from_le_bytes([body[6], body[7]]) as usize;
                if sub_tag != 0x0001 || sub_len != 24 {
                    return Err("升级 ZIP NTFS 子属性类型或长度无效。".into());
                }
            }
            _ => unreachable!(),
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
                layout::validate_release_dir_path(&name)?;
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
                    if name.to_ascii_lowercase().ends_with(".json") {
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
    let mut visited_dirs = Vec::new();
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
                layout::validate_release_dir_path(&name)?;
                visited_dirs.push(name);
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
            if name.to_ascii_lowercase().ends_with(".json") {
                let bytes = storage
                    .read_limited(&item.path(), 8 * 1024 * 1024)?
                    .ok_or("发行 JSON 文件消失。")?;
                let value = crate::json::parse(&bytes)?;
                check_json(&value)?;
            }
            sums.insert(name, format!("{:x}", hash.finalize()));
        }
    }
    for dir in &visited_dirs {
        let prefix = format!("{dir}/");
        if !sums.keys().any(|f| f.starts_with(&prefix)) {
            return Err(format!("发行目录包含空目录或未受管目录：{dir}"));
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
    use std::fs::File;

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

    fn build_raw_stored_zip(entries: &[(&str, &str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cd = Vec::new();
        for &(lfh_name, cd_name, data) in entries {
            let local_off = out.len() as u32;
            let lfh_bytes = lfh_name.as_bytes();
            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed
            out.extend_from_slice(&0u16.to_le_bytes()); // flags
            out.extend_from_slice(&0u16.to_le_bytes()); // stored
            out.extend_from_slice(&0u32.to_le_bytes()); // time/date
            out.extend_from_slice(&0u32.to_le_bytes()); // crc32
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(lfh_bytes.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra len
            out.extend_from_slice(lfh_bytes);
            out.extend_from_slice(data);

            let cd_bytes = cd_name.as_bytes();
            cd.extend_from_slice(b"PK\x01\x02");
            cd.extend_from_slice(&20u16.to_le_bytes()); // version made by
            cd.extend_from_slice(&20u16.to_le_bytes()); // version needed
            cd.extend_from_slice(&0u16.to_le_bytes()); // flags
            cd.extend_from_slice(&0u16.to_le_bytes()); // stored
            cd.extend_from_slice(&0u32.to_le_bytes()); // time/date
            cd.extend_from_slice(&0u32.to_le_bytes()); // crc32
            cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(cd_bytes.len() as u16).to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes()); // extra len
            cd.extend_from_slice(&0u16.to_le_bytes()); // comment len
            cd.extend_from_slice(&0u16.to_le_bytes()); // disk start
            cd.extend_from_slice(&0u16.to_le_bytes()); // internal attr
            cd.extend_from_slice(&0u32.to_le_bytes()); // external attr
            cd.extend_from_slice(&local_off.to_le_bytes());
            cd.extend_from_slice(cd_bytes);
        }
        let cd_offset = out.len() as u32;
        let cd_size = cd.len() as u32;
        let count = entries.len() as u16;
        out.extend_from_slice(&cd);
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    #[test]
    fn zip_bounds_rejects_duplicate_cd_entries_and_lfh_name_spoofing() {
        let dir = std::env::temp_dir().join(format!("cst-zip-bounds-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        let good_path = dir.join("good.zip");
        fs::write(
            &good_path,
            build_raw_stored_zip(&[(
                "cst-pilot/VERSION",
                "cst-pilot/VERSION",
                b"cst-pilot v1.0.0\n",
            )]),
        )
        .unwrap();
        assert!(zip_bounds(&mut File::open(&good_path).unwrap()).is_ok());

        let dup_path = dir.join("dup.zip");
        fs::write(
            &dup_path,
            build_raw_stored_zip(&[
                ("cst-pilot/VERSION", "cst-pilot/VERSION", b"evil"),
                ("cst-pilot/VERSION", "cst-pilot/VERSION", b"clean"),
            ]),
        )
        .unwrap();
        let dup_err = zip_bounds(&mut File::open(&dup_path).unwrap()).unwrap_err();
        assert!(dup_err.contains("重复"));

        let spoof_path = dir.join("spoof.zip");
        fs::write(
            &spoof_path,
            build_raw_stored_zip(&[(
                "cst-pilot/data/config/auth.json",
                "cst-pilot/VERSION",
                b"spoof",
            )]),
        )
        .unwrap();
        let spoof_err = zip_bounds(&mut File::open(&spoof_path).unwrap()).unwrap_err();
        assert!(spoof_err.contains("不一致"));

        // Unsolicited 24-byte 0x0001 ZIP64 extra field on a 32-bit CD entry must be rejected
        // so `zip 6.0.0` cannot be redirected away from the verified 32-bit LFH offset.
        let mut base = build_raw_stored_zip(&[(
            "cst-pilot/VERSION",
            "cst-pilot/VERSION",
            b"cst-pilot v1.0.0\n",
        )]);
        let eocd_pos = base.len() - 22;
        let cd_off =
            u32::from_le_bytes(base[eocd_pos + 16..eocd_pos + 20].try_into().unwrap()) as usize;
        let mut z64_extra = Vec::new();
        z64_extra.extend_from_slice(&0x0001u16.to_le_bytes());
        z64_extra.extend_from_slice(&24u16.to_le_bytes());
        z64_extra.extend_from_slice(&17u64.to_le_bytes());
        z64_extra.extend_from_slice(&17u64.to_le_bytes());
        z64_extra.extend_from_slice(&0u64.to_le_bytes());
        let mut z64_spoof = base[..eocd_pos].to_vec();
        let extra_len_pos = cd_off + 30;
        z64_spoof[extra_len_pos..extra_len_pos + 2]
            .copy_from_slice(&(z64_extra.len() as u16).to_le_bytes());
        z64_spoof.extend_from_slice(&z64_extra);
        let new_cd_size = (z64_spoof.len() - cd_off) as u32;
        base[eocd_pos + 12..eocd_pos + 16].copy_from_slice(&new_cd_size.to_le_bytes());
        z64_spoof.extend_from_slice(&base[eocd_pos..]);
        let z64_path = dir.join("z64-spoof.zip");
        fs::write(&z64_path, &z64_spoof).unwrap();
        let z64_err = zip_bounds(&mut File::open(&z64_path).unwrap()).unwrap_err();
        assert!(z64_err.contains("ZIP64"));

        // Concatenated / multi-EOCD archive (non-contiguous prefix before CD/LFH spans) must be rejected.
        let mut polyglot = build_raw_stored_zip(&[
            ("cst-pilot/VERSION", "cst-pilot/VERSION", b"evil"),
            ("cst-pilot/VERSION", "cst-pilot/VERSION", b"clean"),
        ]);
        let prefix_len = polyglot.len() as u32;
        let mut tail_zip = build_raw_stored_zip(&[(
            "cst-pilot/VERSION",
            "cst-pilot/VERSION",
            b"cst-pilot v1.0.0\n",
        )]);
        let t_eocd = tail_zip.len() - 22;
        let old_cd_off = u32::from_le_bytes(tail_zip[t_eocd + 16..t_eocd + 20].try_into().unwrap());
        let old_lfh_off = u32::from_le_bytes(
            tail_zip[old_cd_off as usize + 42..old_cd_off as usize + 46]
                .try_into()
                .unwrap(),
        );
        tail_zip[old_cd_off as usize + 42..old_cd_off as usize + 46]
            .copy_from_slice(&(old_lfh_off + prefix_len).to_le_bytes());
        tail_zip[t_eocd + 16..t_eocd + 20]
            .copy_from_slice(&(old_cd_off + prefix_len).to_le_bytes());
        polyglot.extend_from_slice(&tail_zip);
        let poly_path = dir.join("polyglot.zip");
        fs::write(&poly_path, &polyglot).unwrap();
        assert!(zip_bounds(&mut File::open(&poly_path).unwrap()).is_err());

        // Directory entry carrying hidden compressed payload must be rejected immediately.
        let dir_with_payload = build_raw_stored_zip(&[
            ("cst-pilot/app/", "cst-pilot/app/", b"smuggled-secret"),
            (
                "cst-pilot/VERSION",
                "cst-pilot/VERSION",
                b"cst-pilot v1.0.0\n",
            ),
        ]);
        assert!(validate_zip_bounds_bytes(&dir_with_payload).is_err());

        // Extra field validation (`0x5455` flag/len mismatch and `0x000a` bad tag/len) must fail.
        assert!(check_extra(&[0x55, 0x54, 0x05, 0x00, 0x03, 1, 2, 3, 4], 0, false).is_err());
        assert!(check_extra(&[0x55, 0x54, 0x05, 0x00, 0x03, 1, 2, 3, 4], 0, true).is_ok());
        assert!(check_extra(&[0x55, 0x54, 0x05, 0x00, 0x08, 1, 2, 3, 4], 0, true).is_err());
        let mut valid_ntfs = vec![0x0a, 0x00, 32, 0, 0, 0, 0, 0, 0x01, 0x00, 24, 0];
        valid_ntfs.extend_from_slice(&[0x11; 24]);
        assert!(check_extra(&valid_ntfs, 0, false).is_ok());
        let mut bad_ntfs = valid_ntfs.clone();
        bad_ntfs[8] = 0x02;
        assert!(check_extra(&bad_ntfs, 0, false).is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_level_zip64_entry_and_telemetry_dir_redlines_are_enforced() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        use ed25519_dalek::{Signer as _, SigningKey};

        let dir = std::env::temp_dir().join(format!("cst-zip64-file-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        // Build a valid 2-entry archive (`VERSION` + `app/SHA256SUMS`) where `VERSION` uses
        // genuine file-level `0x0001` ZIP64 extra fields (`0xffff_ffff` in 32-bit size fields)
        // in both Local File Header and Central Directory.
        let crc32_ieee = |data: &[u8]| -> u32 {
            let mut crc = 0xffff_ffffu32;
            for &b in data {
                crc ^= u32::from(b);
                for _ in 0..8 {
                    let mask = (crc & 1).wrapping_neg();
                    crc = (crc >> 1) ^ (0xedb8_8320 & mask);
                }
            }
            !crc
        };
        let ver_body = b"cst-pilot v1.2.3\n";
        let crc_ver = crc32_ieee(ver_body);
        let sums_body = format!("{:x}  VERSION\n", Sha256::digest(ver_body));
        let crc_sums = crc32_ieee(sums_body.as_bytes());

        let mut zip64_archive = Vec::new();
        let mut cd = Vec::new();

        // Entry 0: `cst-pilot/VERSION` with 16-byte `0x0001` ZIP64 in LFH & CD
        let name0 = b"cst-pilot/VERSION";
        let mut z64_sizes = Vec::new();
        z64_sizes.extend_from_slice(&0x0001u16.to_le_bytes());
        z64_sizes.extend_from_slice(&16u16.to_le_bytes());
        z64_sizes.extend_from_slice(&(ver_body.len() as u64).to_le_bytes());
        z64_sizes.extend_from_slice(&(ver_body.len() as u64).to_le_bytes());
        // Also attach an Info-ZIP `0x5455` timestamp block where flags = 0x03 (mtime + atime):
        // LFH carries 9 bytes (flags + mtime + atime), while CD carries 5 bytes (flags + mtime).
        let mut lfh_extra0 = z64_sizes.clone();
        lfh_extra0.extend_from_slice(&[0x55, 0x54, 9, 0, 0x03, 1, 2, 3, 4, 5, 6, 7, 8]);
        let mut cd_extra0 = z64_sizes.clone();
        cd_extra0.extend_from_slice(&[0x55, 0x54, 5, 0, 0x03, 1, 2, 3, 4]);

        let off0 = zip64_archive.len() as u32;
        zip64_archive.extend_from_slice(b"PK\x03\x04");
        zip64_archive.extend_from_slice(&45u16.to_le_bytes());
        zip64_archive.extend_from_slice(&0u16.to_le_bytes());
        zip64_archive.extend_from_slice(&0u16.to_le_bytes());
        zip64_archive.extend_from_slice(&0u32.to_le_bytes());
        zip64_archive.extend_from_slice(&crc_ver.to_le_bytes());
        zip64_archive.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
        zip64_archive.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
        zip64_archive.extend_from_slice(&(name0.len() as u16).to_le_bytes());
        zip64_archive.extend_from_slice(&(lfh_extra0.len() as u16).to_le_bytes());
        zip64_archive.extend_from_slice(name0);
        zip64_archive.extend_from_slice(&lfh_extra0);
        zip64_archive.extend_from_slice(ver_body);

        cd.extend_from_slice(b"PK\x01\x02");
        cd.extend_from_slice(&45u16.to_le_bytes());
        cd.extend_from_slice(&45u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u32.to_le_bytes());
        cd.extend_from_slice(&crc_ver.to_le_bytes());
        cd.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
        cd.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
        cd.extend_from_slice(&(name0.len() as u16).to_le_bytes());
        cd.extend_from_slice(&(cd_extra0.len() as u16).to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u32.to_le_bytes());
        cd.extend_from_slice(&off0.to_le_bytes());
        cd.extend_from_slice(name0);
        cd.extend_from_slice(&cd_extra0);

        // Entry 1: `cst-pilot/app/SHA256SUMS`
        let name1 = b"cst-pilot/app/SHA256SUMS";
        let off1 = zip64_archive.len() as u32;
        zip64_archive.extend_from_slice(b"PK\x03\x04");
        zip64_archive.extend_from_slice(&20u16.to_le_bytes());
        zip64_archive.extend_from_slice(&0u16.to_le_bytes());
        zip64_archive.extend_from_slice(&0u16.to_le_bytes());
        zip64_archive.extend_from_slice(&0u32.to_le_bytes());
        zip64_archive.extend_from_slice(&crc_sums.to_le_bytes());
        zip64_archive.extend_from_slice(&(sums_body.len() as u32).to_le_bytes());
        zip64_archive.extend_from_slice(&(sums_body.len() as u32).to_le_bytes());
        zip64_archive.extend_from_slice(&(name1.len() as u16).to_le_bytes());
        zip64_archive.extend_from_slice(&0u16.to_le_bytes());
        zip64_archive.extend_from_slice(name1);
        zip64_archive.extend_from_slice(sums_body.as_bytes());

        cd.extend_from_slice(b"PK\x01\x02");
        cd.extend_from_slice(&20u16.to_le_bytes());
        cd.extend_from_slice(&20u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u32.to_le_bytes());
        cd.extend_from_slice(&crc_sums.to_le_bytes());
        cd.extend_from_slice(&(sums_body.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(sums_body.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(name1.len() as u16).to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u32.to_le_bytes());
        cd.extend_from_slice(&off1.to_le_bytes());
        cd.extend_from_slice(name1);

        let cd_off = zip64_archive.len() as u32;
        let cd_sz = cd.len() as u32;
        zip64_archive.extend_from_slice(&cd);
        zip64_archive.extend_from_slice(b"PK\x05\x06");
        zip64_archive.extend_from_slice(&0u16.to_le_bytes());
        zip64_archive.extend_from_slice(&0u16.to_le_bytes());
        zip64_archive.extend_from_slice(&2u16.to_le_bytes());
        zip64_archive.extend_from_slice(&2u16.to_le_bytes());
        zip64_archive.extend_from_slice(&cd_sz.to_le_bytes());
        zip64_archive.extend_from_slice(&cd_off.to_le_bytes());
        zip64_archive.extend_from_slice(&0u16.to_le_bytes());

        let out_clean = dir.join("out-clean");
        fs::create_dir_all(&out_clean).unwrap();
        let unpacked =
            extract_clean_dist_archive_bytes(&zip64_archive, &out_clean, 1024 * 1024).unwrap();
        assert_eq!(unpacked, (ver_body.len() + sums_body.len()) as u64);
        assert_eq!(fs::read(out_clean.join("VERSION")).unwrap(), ver_body);

        let signing = SigningKey::from_bytes(&[7u8; 32]);
        let rkey = ReleaseKey::from_bytes(&signing.verifying_key().to_bytes()).unwrap();
        let sums_sig = STANDARD.encode(
            signing
                .sign(&[crate::manifest::SUMS_DOMAIN, sums_body.as_bytes()].concat())
                .to_bytes(),
        );
        let manifest = crate::manifest::ReleaseManifest {
            schema_version: 1,
            platform: "windows-x86_64".into(),
            version: "v1.2.3".into(),
            min_upgradable_version: "v1.0.0".into(),
            built_at: "2026-11-27T00:00:00Z".into(),
            release_notes: String::new(),
            archive: crate::manifest::Archive {
                url: "https://www.cstoa.top/api/agent/releases/files/v1.2.3/cst-pilot-v1.2.3-windows-x64.zip".into(),
                size: zip64_archive.len() as u64,
                unpacked_size: unpacked,
                sha256: format!("{:x}", Sha256::digest(&zip64_archive)),
            },
            sha256_sums: sums_body,
            sha256_signature: sums_sig,
        };
        let payload = serde_json::to_string(&manifest).unwrap();
        let sig = STANDARD.encode(
            signing
                .sign(&[crate::manifest::MANIFEST_DOMAIN, payload.as_bytes()].concat())
                .to_bytes(),
        );
        let env_bytes = serde_json::to_vec(&crate::manifest::SignedManifest {
            payload,
            signature: sig,
        })
        .unwrap();
        let verified = rkey.verify(&env_bytes).unwrap();
        let out_upd = dir.join("out-upd");
        fs::create_dir_all(&out_upd).unwrap();
        extract_archive_bytes(
            &zip64_archive,
            &out_upd,
            &verified,
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(fs::read(out_upd.join("VERSION")).unwrap(), ver_body);

        // Also verify `validate_release_dir_path` blocks `app/telemetry/drafts`.
        assert!(layout::validate_release_dir_path("app/telemetry").is_ok());
        assert!(layout::validate_release_dir_path("app/telemetry/drafts").is_err());

        let _ = fs::remove_dir_all(&dir);
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
                        | "secretkey"
                        | "privatekey"
                        | "signingkey"
                        | "password"
                        | "clientsecret"
                        | "authorization"
                        | "identitykey"
                ) && is_non_empty_credential_json_value(value)
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

fn is_non_empty_credential_json_value(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(_) => true,
        Value::Number(_) => true,
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
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

use super::{
    io,
    layout::{self, Area},
    lock::UpdateLock,
    manifest::{MAX_FILES, ReleaseKey, VerifiedRelease},
    verify::{self, VerifiedPackage},
};
use crate::runbook::storage::Storage;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

const JOURNAL: &str = ".state/update/transaction.json";
const JOURNAL_LIMIT: usize = 64 * 1024 * 1024;

/// Implemented only by a trusted host. Neither executable nor command line comes from a manifest.
pub trait UpdateHost: Send + Sync {
    /// Must terminate the health-check process and its descendants before returning a failure.
    fn health_check(&self, root: &Path, cancel: &CancellationToken) -> Result<(), String>;
    fn restart(&self, root: &Path) -> Result<(), String>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyStep {
    JournalSaved,
    BackedUp(usize),
    Installed(usize),
    Healthy,
    Committed,
}
pub trait ApplyObserver {
    fn reached(&self, step: ApplyStep) -> Result<(), String>;
}
impl ApplyObserver for () {
    fn reached(&self, _: ApplyStep) -> Result<(), String> {
        Ok(())
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    Updated { restarted: bool },
    RolledBack,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RollbackReason {
    Installation,
    HealthCheck,
    Cancelled,
    Interrupted,
}
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateReport {
    pub version: String,
    pub rolled_back: bool,
    pub reason: Option<RollbackReason>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Recovery {
    None,
    RolledBack,
    CompletedCleanup,
}

/// Holds the cross-process lock until all file workers, health checks and rollback have finished.
pub struct Applier {
    storage: Storage,
    key: ReleaseKey,
    _lock: UpdateLock,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Journal {
    v: u32,
    id: String,
    stage: String,
    envelope: String,
    committed: bool,
    entries: Vec<Entry>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Entry {
    path: String,
    old: Option<String>,
    next: Option<String>,
    merged: bool,
}
struct UnpublishedBackup {
    storage: Storage,
    relative: PathBuf,
    published: bool,
}
impl Drop for UnpublishedBackup {
    fn drop(&mut self) {
        if !self.published {
            let _ = io::clean(&self.storage, &self.relative);
        }
    }
}
impl Journal {
    fn base(&self) -> PathBuf {
        PathBuf::from(format!(".state/update/backup/{}", self.id))
    }
    fn backup(&self, name: &str) -> PathBuf {
        self.base().join("files").join(name)
    }
    fn prepared(&self, name: &str) -> PathBuf {
        self.base().join("prepared").join(name)
    }
}
impl Applier {
    pub fn new(root: &Path, key: ReleaseKey) -> Result<Self, String> {
        let storage = Storage::new(root)?;
        let lock = UpdateLock::acquire(&storage.root)?;
        Ok(Self {
            storage,
            key,
            _lock: lock,
        })
    }
    pub fn root(&self) -> &Path {
        &self.storage.root
    }
    pub fn pending(&self) -> Result<bool, String> {
        Ok(self.read_journal()?.is_some())
    }
    pub fn recover(&self) -> Result<Recovery, String> {
        let Some(journal) = self.read_journal()? else {
            return Ok(Recovery::None);
        };
        if journal.committed {
            self.save_report(&journal, None)?;
            self.cleanup(&journal)?;
            Ok(Recovery::CompletedCleanup)
        } else {
            self.rollback(&journal, RollbackReason::Interrupted)?;
            Ok(Recovery::RolledBack)
        }
    }
    pub fn apply(
        &self,
        mut package: VerifiedPackage,
        host: &dyn UpdateHost,
        cancel: &CancellationToken,
    ) -> Result<ApplyOutcome, String> {
        self.apply_observed(&mut package, host, &(), cancel)
    }
    /// Observer failures trigger rollback. An abrupt process exit is recovered from the saved journal.
    pub fn apply_observed(
        &self,
        package: &mut VerifiedPackage,
        host: &dyn UpdateHost,
        observer: &dyn ApplyObserver,
        cancel: &CancellationToken,
    ) -> Result<ApplyOutcome, String> {
        package.revalidate(&self.key, cancel)?;
        let relative = package
            .stage
            .root
            .strip_prefix(&self.storage.root)
            .map_err(|_| "升级包不属于当前工具包。")?
            .to_str()
            .ok_or("升级暂存路径无效。")?
            .replace('\\', "/");
        validate_stage(&relative, &package.release)?;
        self.preflight(&package.release, cancel)?;
        let journal = self.plan(&relative, &package.release, cancel)?;
        self.save_journal(&journal)?;
        package.stage.retain = true;
        self.execute(journal, host, observer, cancel)
    }
    /// Independent updater entry: stage names are constrained to the kit's staging directory.
    pub fn apply_stage(
        &self,
        relative: &str,
        host: &dyn UpdateHost,
        cancel: &CancellationToken,
    ) -> Result<ApplyOutcome, String> {
        let release = self.staged_release(relative, cancel)?;
        self.preflight(&release, cancel)?;
        let journal = self.plan(relative, &release, cancel)?;
        self.save_journal(&journal)?;
        self.execute(journal, host, &(), cancel)
    }
    pub(super) fn staged_release(
        &self,
        relative: &str,
        cancel: &CancellationToken,
    ) -> Result<VerifiedRelease, String> {
        // Validate a bounded lexical prefix before any filesystem access.
        stage_parts(relative)?;
        let _pin = self.storage.pin_directory(Path::new(relative), false)?;
        let bytes = self
            .storage
            .read_limited(
                &self.storage.root.join(relative).join("manifest.json"),
                super::manifest::MAX_MANIFEST_BYTES,
            )?
            .ok_or("缺少暂存签名清单。")?;
        let release = self.key.verify(&bytes)?;
        validate_stage(relative, &release)?;
        verify::verify_tree(
            &self.storage.root.join(relative).join("program"),
            &release,
            cancel,
        )?;
        Ok(release)
    }
    fn preflight(
        &self,
        release: &VerifiedRelease,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        io::cancelled(cancel)?;
        if self.read_journal()?.is_some() {
            return Err("存在未完成的升级事务，请先恢复。".into());
        }
        if !release.available_from(&layout::read_version(self.root())?)? {
            return Err("升级版本未高于当前版本。".into());
        }
        if crate::bitlocker::lock::VolumeLock::active_count() > 0 {
            return Err("BitLocker 操作尚未结束，暂不能升级。".into());
        }
        Ok(())
    }
    fn plan(
        &self,
        stage: &str,
        release: &VerifiedRelease,
        cancel: &CancellationToken,
    ) -> Result<Journal, String> {
        let mut paths = inventory(&self.storage, cancel)?;
        paths.extend(release.sums.keys().cloned());
        paths.insert("SHA256SUMS".into());
        let mut journal = Journal {
            v: 1,
            id: uuid::Uuid::new_v4().to_string(),
            stage: stage.into(),
            envelope: String::from_utf8(release.envelope().to_vec())
                .map_err(|_| "签名清单编码无效。")?,
            committed: false,
            entries: Vec::new(),
        };
        let mut cleanup = UnpublishedBackup {
            storage: self.storage.clone(),
            relative: journal.base(),
            published: false,
        };
        let _base = self.storage.pin_directory(&journal.base(), true)?;
        for name in paths {
            io::cancelled(cancel)?;
            let merged = layout::classify(&name)? == Area::Merge;
            let path = self.storage.root.join(&name);
            let _pin = io::parent_pin(&self.storage, &path, true)?;
            let old = io::hash(&self.storage, &path, cancel)?;
            let signed = if name == "SHA256SUMS" {
                Some(format!(
                    "{:x}",
                    Sha256::digest(release.info.sha256_sums.as_bytes())
                ))
            } else {
                release.sums.get(&name).cloned()
            };
            let next = if merged {
                let Some(_) = signed else { continue };
                let current = self.storage.read_limited(&path, 8 * 1024 * 1024)?;
                let defaults = self
                    .storage
                    .read_limited(
                        &self.storage.root.join(stage).join("program").join(&name),
                        8 * 1024 * 1024,
                    )?
                    .ok_or("预置配置已消失。")?;
                let bytes = layout::merge_defaults(&name, current.as_deref(), &defaults)?;
                let prepared = self.storage.root.join(journal.prepared(&name));
                let _pin = io::parent_pin(&self.storage, &prepared, true)?;
                self.storage.atomic(&prepared, &bytes, false, cancel)?;
                Some(format!("{:x}", Sha256::digest(&bytes)))
            } else {
                signed
            };
            if old != next {
                journal.entries.push(Entry {
                    path: name,
                    old,
                    next,
                    merged,
                });
            }
        }
        // VERSION is published last; the journal remains authoritative across every move.
        journal
            .entries
            .sort_by_key(|e| (e.path == "VERSION", e.path.clone()));
        if journal.entries.len() > MAX_FILES * 2 {
            return Err("升级事务文件过多。".into());
        }
        cleanup.published = true;
        Ok(journal)
    }
    fn execute(
        &self,
        mut journal: Journal,
        host: &dyn UpdateHost,
        observer: &dyn ApplyObserver,
        cancel: &CancellationToken,
    ) -> Result<ApplyOutcome, String> {
        let health_started = std::cell::Cell::new(false);
        let run = || -> Result<(), String> {
            observer.reached(ApplyStep::JournalSaved)?;
            for (index, entry) in journal.entries.iter().enumerate() {
                io::cancelled(cancel)?;
                let target = self.storage.root.join(&entry.path);
                if let Some(old) = &entry.old {
                    io::move_file(
                        &self.storage,
                        &target,
                        &self.storage.root.join(journal.backup(&entry.path)),
                        old,
                        cancel,
                    )?;
                } else {
                    io::same(&self.storage, &target, None, cancel)?;
                }
                observer.reached(ApplyStep::BackedUp(index))?;
                if let Some(next) = &entry.next {
                    let source = if entry.merged {
                        self.storage.root.join(journal.prepared(&entry.path))
                    } else {
                        self.storage
                            .root
                            .join(&journal.stage)
                            .join("program")
                            .join(&entry.path)
                    };
                    io::move_file(&self.storage, &source, &target, next, cancel)?;
                }
                observer.reached(ApplyStep::Installed(index))?;
            }
            // Recheck the entire transaction before executing the new program.
            for entry in &journal.entries {
                io::same(
                    &self.storage,
                    &self.storage.root.join(&entry.path),
                    entry.next.as_deref(),
                    cancel,
                )?;
            }
            let release = self.key.verify(journal.envelope.as_bytes())?;
            for (path, expected) in &release.sums {
                if layout::classify(path)? == Area::Program {
                    let target = self.storage.root.join(path);
                    let _pin = io::parent_pin(&self.storage, &target, false)?;
                    io::same(&self.storage, &target, Some(expected), cancel)?;
                }
            }
            health_started.set(true);
            host.health_check(self.root(), cancel)?;
            observer.reached(ApplyStep::Healthy)?;
            Ok(())
        };
        if run().is_err() {
            // Rollback uses an independent token, including after caller cancellation.
            let reason = if cancel.is_cancelled() {
                RollbackReason::Cancelled
            } else if health_started.get() {
                RollbackReason::HealthCheck
            } else {
                RollbackReason::Installation
            };
            self.rollback(&journal, reason)?;
            return Ok(ApplyOutcome::RolledBack);
        }
        journal.committed = true;
        self.save_journal(&journal)?;
        self.save_report(&journal, None)?;
        observer.reached(ApplyStep::Committed)?;
        self.cleanup(&journal)?;
        Ok(ApplyOutcome::Updated {
            restarted: host.restart(self.root()).is_ok(),
        })
    }
    fn rollback(&self, journal: &Journal, reason: RollbackReason) -> Result<(), String> {
        let cancel = CancellationToken::new();
        for entry in journal.entries.iter().rev() {
            let target = self.storage.root.join(&entry.path);
            let _target = io::parent_pin(&self.storage, &target, false)?;
            let backup = self.storage.root.join(journal.backup(&entry.path));
            let _backup = io::parent_pin(&self.storage, &backup, true)?;
            let current = io::hash(&self.storage, &target, &cancel)?;
            let saved = io::hash(&self.storage, &backup, &cancel)?;
            match (entry.old.as_deref(), saved.as_deref()) {
                (Some(old), Some(saved)) if old == saved => {
                    if current.as_deref() == Some(old) {
                        // A completed restore interrupted before backup removal.
                        io::remove(&self.storage, &backup, old, &cancel)?;
                        continue;
                    }
                    if current.is_some() {
                        if current != entry.next {
                            return Err("回滚目标已被修改，原文件和备份均已保留。".into());
                        }
                        io::remove(
                            &self.storage,
                            &target,
                            current.as_deref().ok_or("回滚哈希缺失。")?,
                            &cancel,
                        )?;
                    }
                    io::move_file(&self.storage, &backup, &target, old, &cancel)?;
                }
                (Some(old), None) if current.as_deref() == Some(old) => (),
                (None, None) => {
                    if let Some(current) = current {
                        if Some(&current) != entry.next.as_ref() {
                            return Err("新程序文件已被修改，回滚已暂停。".into());
                        }
                        io::remove(&self.storage, &target, &current, &cancel)?;
                    }
                }
                _ => return Err("回滚备份缺失或已修改，事务已保留。".into()),
            }
        }
        self.save_report(journal, Some(reason))?;
        self.cleanup(journal)
    }
    /// A fixed local result for startup UI; no command, path, credential, or raw error is stored.
    pub fn report(&self) -> Result<Option<UpdateReport>, String> {
        if !self
            .storage
            .root
            .join(".state/update")
            .try_exists()
            .map_err(|_| "升级结果无法读取。")?
        {
            return Ok(None);
        }
        let _pin = self
            .storage
            .pin_directory(Path::new(".state/update"), false)?;
        let Some(bytes) = self
            .storage
            .read_limited(&self.storage.root.join(".state/update/result.json"), 4096)?
        else {
            return Ok(None);
        };
        let report: UpdateReport = serde_json::from_value(super::json::parse(&bytes)?)
            .map_err(|_| "升级结果格式无效。")?;
        super::manifest::version(&report.version)?;
        if report.rolled_back != report.reason.is_some() {
            return Err("升级结果字段无效。".into());
        }
        Ok(Some(report))
    }
    fn save_report(&self, journal: &Journal, reason: Option<RollbackReason>) -> Result<(), String> {
        let release = self.key.verify(journal.envelope.as_bytes())?;
        let report = UpdateReport {
            version: release.info.version,
            rolled_back: reason.is_some(),
            reason,
        };
        let bytes = serde_json::to_vec(&report).map_err(|_| "升级结果无法编码。")?;
        let _pin = self
            .storage
            .pin_directory(Path::new(".state/update"), false)?;
        self.storage.atomic(
            &self.storage.root.join(".state/update/result.json"),
            &bytes,
            true,
            &CancellationToken::new(),
        )
    }
    fn save_journal(&self, journal: &Journal) -> Result<(), String> {
        let bytes = serde_json::to_vec(journal).map_err(|_| "升级事务无法编码。")?;
        if bytes.len() > JOURNAL_LIMIT {
            return Err("升级事务超过大小限制。".into());
        }
        let _pin = self
            .storage
            .pin_directory(Path::new(".state/update"), true)?;
        self.storage.atomic(
            &self.storage.root.join(JOURNAL),
            &bytes,
            true,
            &CancellationToken::new(),
        )
    }
    fn read_journal(&self) -> Result<Option<Journal>, String> {
        let parent = self.storage.root.join(".state/update");
        if !parent.try_exists().map_err(|_| "升级事务目录无法读取。")? {
            return Ok(None);
        }
        let _pin = self
            .storage
            .pin_directory(Path::new(".state/update"), false)?;
        let Some(bytes) = self
            .storage
            .read_limited(&self.storage.root.join(JOURNAL), JOURNAL_LIMIT)?
        else {
            return Ok(None);
        };
        let journal: Journal = serde_json::from_value(super::json::parse(&bytes)?)
            .map_err(|_| "升级事务损坏，原文件已保留。")?;
        let release = self.key.verify(journal.envelope.as_bytes())?;
        if journal.v != 1
            || uuid::Uuid::parse_str(&journal.id)
                .map(|v| v.to_string())
                .ok()
                .as_deref()
                != Some(&journal.id)
            || journal.entries.len() > MAX_FILES * 2
        {
            return Err("升级事务无效。".into());
        }
        validate_stage(&journal.stage, &release)?;
        let mut seen = BTreeSet::new();
        for entry in &journal.entries {
            let area = layout::classify(&entry.path)?;
            if !seen.insert(entry.path.to_lowercase())
                || entry.merged != (area == Area::Merge)
                || entry.old.as_ref().is_some_and(|h| !valid_hash(h))
                || entry.next.as_ref().is_some_and(|h| !valid_hash(h))
            {
                return Err("升级事务条目无效。".into());
            }
            if !entry.merged {
                let signed = if entry.path == "SHA256SUMS" {
                    Some(format!(
                        "{:x}",
                        Sha256::digest(release.info.sha256_sums.as_bytes())
                    ))
                } else {
                    release.sums.get(&entry.path).cloned()
                };
                if signed != entry.next {
                    return Err("升级事务与签名清单不一致。".into());
                }
            }
        }
        Ok(Some(journal))
    }
    fn cleanup(&self, journal: &Journal) -> Result<(), String> {
        // Delete the marker only after its backup and stage have been safely removed.
        // For rollback, missing already-restored backups are accepted on the next recovery.
        io::clean(&self.storage, &journal.base())?;
        io::clean(&self.storage, Path::new(&journal.stage))?;
        let path = self.storage.root.join(JOURNAL);
        let _pin = io::parent_pin(&self.storage, &path, false)?;
        fs::remove_file(path).map_err(|_| "升级事务标记无法清理。".into())
    }
}
fn valid_hash(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn stage_parts(relative: &str) -> Result<(), String> {
    layout::safe_path(relative)?;
    if relative.split('/').count() != 4 || !relative.starts_with(".state/update/staging/") {
        return Err("升级暂存路径无效。".into());
    }
    Ok(())
}
fn validate_stage(relative: &str, release: &VerifiedRelease) -> Result<(), String> {
    stage_parts(relative)?;
    let leaf = relative.rsplit('/').next().ok_or("升级暂存路径无效。")?;
    let id = leaf
        .strip_prefix(&format!("{}-", release.info.version))
        .ok_or("升级暂存版本无效。")?;
    if uuid::Uuid::parse_str(id)
        .map(|v| v.to_string())
        .ok()
        .as_deref()
        != Some(id)
    {
        return Err("升级暂存标识无效。".into());
    }
    Ok(())
}
fn inventory(storage: &Storage, cancel: &CancellationToken) -> Result<BTreeSet<String>, String> {
    let mut result = BTreeSet::new();
    let mut pending = vec![PathBuf::new()];
    let mut count = 0usize;
    while let Some(relative) = pending.pop() {
        let _pin = storage.pin_directory(&relative, false)?;
        for item in
            fs::read_dir(storage.root.join(&relative)).map_err(|_| "旧程序目录无法读取。")?
        {
            io::cancelled(cancel)?;
            count += 1;
            if count > MAX_FILES * 3 {
                return Err("旧程序条目超过升级限制。".into());
            }
            let item = item.map_err(|_| "旧程序条目无法读取。")?;
            let rel = relative.join(item.file_name());
            let name = rel.to_str().ok_or("旧程序文件名无效。")?.replace('\\', "/");
            let meta = fs::symlink_metadata(item.path()).map_err(|_| "旧程序文件无法检查。")?;
            if meta.is_dir() {
                let descend = matches!(name.as_str(), "agent" | "agent/home")
                    || layout::classify(&format!("{name}/cst-upgrade-probe.bin")).is_ok();
                if descend {
                    pending.push(rel)
                }
            } else if layout::classify(&name).is_ok() {
                // open_read rejects symbolic links, hard links, and non-regular files.
                io::hash(storage, &item.path(), cancel)?.ok_or("旧程序文件已消失。")?;
                result.insert(name);
            }
        }
    }
    Ok(result)
}

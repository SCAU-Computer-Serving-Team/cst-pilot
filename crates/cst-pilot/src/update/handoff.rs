use super::{
    applier::{Applier, ApplyOutcome, Recovery, UpdateHost},
    io, layout,
    manifest::ReleaseKey,
    process::{self, NativeHost, ProcessIdentity, RestartUi},
    verify::VerifiedPackage,
};
use crate::runbook::storage::Storage;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    v: u32,
    id: String,
    stage: String,
    parents: Vec<ProcessIdentity>,
    ui: RestartUi,
}
/// Owns the prepared package until the independent updater has been started successfully.
pub struct Handoff {
    storage: Storage,
    package: VerifiedPackage,
    request: Request,
    updater: PathBuf,
    launched: bool,
}
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchReceipt {
    pub updater_pid: u32,
    pub request_id: String,
}
impl Handoff {
    pub fn prepare(
        root: &Path,
        package: VerifiedPackage,
        key: &ReleaseKey,
        parents: Vec<ProcessIdentity>,
        ui: RestartUi,
        cancel: &CancellationToken,
    ) -> Result<Self, String> {
        let applier = Applier::new(root, key.clone())?;
        package.revalidate(key, cancel)?;
        if applier.pending()? {
            return Err("存在未完成的升级，请先退出并恢复工具包。".into());
        }
        if parents.is_empty()
            || parents.len() > 16
            || !parents.iter().any(|p| p.pid == std::process::id())
        {
            return Err("升级交接必须包含当前进程。".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for parent in &parents {
            if !seen.insert(parent.pid)
                || ProcessIdentity::capture(parent.pid)?.as_ref() != Some(parent)
            {
                return Err("升级交接进程已变化。".into());
            }
        }
        let storage = Storage::new(root)?;
        let stage = package
            .stage
            .root
            .strip_prefix(&storage.root)
            .map_err(|_| "升级包不属于当前工具包。")?
            .to_str()
            .ok_or("升级暂存路径无效。")?
            .replace('\\', "/");
        applier.staged_release(&stage, cancel)?;
        if !package
            .release
            .available_from(&layout::read_version(root)?)?
        {
            return Err("目标版本未高于当前工具包版本。".into());
        }
        let request = Request {
            v: 1,
            id: uuid::Uuid::new_v4().to_string(),
            stage,
            parents,
            ui,
        };
        let updater = storage
            .root
            .join(format!(".state/update/cst-updater-{}.exe", request.id));
        let expected = package
            .release
            .sums
            .get("cst-updater.exe")
            .ok_or("升级包缺少更新器。")?;
        let _update = storage.pin_directory(Path::new(".state/update"), true)?;
        let _stage =
            storage.pin_directory(Path::new(&request.stage).join("program").as_path(), false)?;
        copy_updater(
            &storage,
            &package.directory().join("cst-updater.exe"),
            &updater,
            expected,
            cancel,
        )?;
        let bytes = serde_json::to_vec(&request).map_err(|_| "升级交接无法编码。")?;
        let _requests = storage.pin_directory(Path::new(".state/update/requests"), true)?;
        if let Err(error) = storage.atomic(
            &storage.root.join(request_path(&request.id)),
            &bytes,
            false,
            cancel,
        ) {
            let _ = fs::remove_file(&updater);
            return Err(error);
        }
        Ok(Self {
            storage,
            package,
            request,
            updater,
            launched: false,
        })
    }
    pub fn request_id(&self) -> &str {
        &self.request.id
    }
    pub fn updater_path(&self) -> &Path {
        &self.updater
    }
    /// The caller must now close every listed host process. This function never exits its host.
    pub fn launch(mut self) -> Result<LaunchReceipt, String> {
        let cancel = CancellationToken::new();
        let _pin = io::parent_pin(&self.storage, &self.updater, false)?;
        let expected = self
            .package
            .release
            .sums
            .get("cst-updater.exe")
            .ok_or("更新器哈希缺失。")?;
        io::same(&self.storage, &self.updater, Some(expected), &cancel)?;
        let pid = process::launch(
            &self.updater,
            &[
                "--root".into(),
                self.storage.root.as_os_str().into(),
                "--request".into(),
                self.request.id.clone().into(),
            ],
            &self.storage.root,
        )?;
        self.package.stage.retain = true;
        self.launched = true;
        Ok(LaunchReceipt {
            updater_pid: pid,
            request_id: self.request.id.clone(),
        })
    }
}
impl Drop for Handoff {
    fn drop(&mut self) {
        if !self.launched {
            // Only known files under pinned ancestors are removed. The package owns stage cleanup.
            let request = self.storage.root.join(request_path(&self.request.id));
            if let Ok(_pin) = io::parent_pin(&self.storage, &request, false) {
                let _ = fs::remove_file(request);
            }
            if let Ok(_pin) = io::parent_pin(&self.storage, &self.updater, false) {
                let _ = fs::remove_file(&self.updater);
            }
        }
    }
}
fn request_path(id: &str) -> PathBuf {
    PathBuf::from(format!(".state/update/requests/{id}.json"))
}
fn copy_updater(
    storage: &Storage,
    from: &Path,
    to: &Path,
    expected: &str,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let mut input = storage.open_read(from)?.ok_or("暂存更新器已消失。")?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
        .map_err(|_| "独立更新器无法创建。")?;
    let result = (|| {
        let mut buffer = [0; 65536];
        loop {
            io::cancelled(cancel)?;
            let n = input
                .read(&mut buffer)
                .map_err(|_| "暂存更新器无法读取。")?;
            if n == 0 {
                break;
            }
            output
                .write_all(&buffer[..n])
                .map_err(|_| "独立更新器写入失败。")?;
        }
        output.sync_all().map_err(|_| "独立更新器无法同步。")?;
        drop(output);
        io::same(storage, to, Some(expected), cancel)
    })();
    if result.is_err() {
        let _ = fs::remove_file(to);
    }
    result
}
/// Production updater entry. Trust comes exclusively from its compiled release key.
pub fn run_request(root: &Path, id: &str, key: ReleaseKey) -> Result<ApplyOutcome, String> {
    if uuid::Uuid::parse_str(id)
        .map(|v| v.to_string())
        .ok()
        .as_deref()
        != Some(id)
    {
        return Err("升级交接标识无效。".into());
    }
    let applier = Applier::new(root, key)?;
    let storage = Storage::new(applier.root())?;
    let _pin = storage.pin_directory(Path::new(".state/update/requests"), false)?;
    let path = storage.root.join(request_path(id));
    let bytes = storage
        .read_limited(&path, 64 * 1024)?
        .ok_or("升级交接记录不存在。")?;
    let request: Request =
        serde_json::from_value(super::json::parse(&bytes)?).map_err(|_| "升级交接记录无效。")?;
    if request.v != 1 || request.id != id {
        return Err("升级交接记录不匹配。".into());
    }
    let cancel = CancellationToken::new();
    let release = applier.staged_release(&request.stage, &cancel)?;
    let own = std::env::current_exe()
        .map_err(|_| "更新器路径无法读取。")?
        .canonicalize()
        .map_err(|_| "更新器路径无法核验。")?;
    let expected_path = storage
        .root
        .join(format!(".state/update/cst-updater-{id}.exe"));
    if own != expected_path {
        return Err("更新器未从已校验的独立路径启动。".into());
    }
    let _updater_pin = io::parent_pin(&storage, &own, false)?;
    io::same(
        &storage,
        &own,
        release.sums.get("cst-updater.exe").map(String::as_str),
        &cancel,
    )?;
    // Capture native handles and compare creation times before waiting; PID reuse never authorizes replacement.
    process::wait_for_parents(&request.parents, Duration::from_secs(60), &cancel)?;
    let host = NativeHost { ui: request.ui };
    let outcome = match applier.recover()? {
        Recovery::CompletedCleanup => ApplyOutcome::Updated {
            restarted: host.restart(applier.root()).is_ok(),
        },
        Recovery::RolledBack => {
            let _ = host.restart(applier.root());
            ApplyOutcome::RolledBack
        }
        Recovery::None => {
            let outcome = applier.apply_stage(&request.stage, &host, &cancel)?;
            if matches!(outcome, ApplyOutcome::RolledBack) {
                let _ = host.restart(applier.root());
            }
            outcome
        }
    };
    io::remove(
        &storage,
        &path,
        &format!("{:x}", sha2::Sha256::digest(&bytes)),
        &cancel,
    )?;
    Ok(outcome)
}
use sha2::Digest;

/// The business executable calls this before starting services when given --health-check.
/// It performs only local checks, without creating credentials, telemetry, or contacting a provider.
pub fn health_check(root: &Path) -> Result<(), String> {
    layout::read_version(root)?;
    crate::Kit::new(root)?.check_resources()?;
    let storage = Storage::new(root)?;
    let _home = storage.pin_directory(Path::new("agent/home"), false)?;
    for name in ["auth.json", "models.json", "settings.json", "device.json"] {
        if let Some(bytes) =
            storage.read_limited(&storage.root.join("agent/home").join(name), 8 * 1024 * 1024)?
        {
            let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
            if !super::json::parse(bytes)?.is_object() {
                return Err("健康检查发现配置格式无效，原文件已保留。".into());
            }
        }
    }
    Ok(())
}

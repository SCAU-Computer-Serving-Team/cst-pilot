use std::fs::OpenOptions;
use std::path::Path;
use std::time::Duration;

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

pub use cst_updater::verify::{Stage, VerifiedPackage, verify_tree};

use crate::update::manifest::VerifiedRelease;
use crate::update::network::Fetcher;

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
    let stage = Stage::create(root, &release)?;
    let pin = stage.base.pin_directory(&stage.relative, false)?;
    let archive = stage.archive_path();
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
        loop {
            stopped(&cancel)?;
            let next = tokio::time::timeout(Duration::from_secs(30), stream.body.next())
                .await
                .map_err(|_| "升级包下载超时（30 秒无数据）。".to_owned())?;
            let Some(chunk) = next else {
                break;
            };
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
        r = tokio::time::timeout(Duration::from_secs(600), download) => r.map_err(|_| "升级包下载超过 10 分钟。".to_owned())?,
    };
    drop(file);
    result?;
    drop(pin);
    stage.write_manifest(&release, &cancel)?;
    let child = cancel.child_token();
    let _cancel_on_drop = child.clone().drop_guard();
    tokio::task::spawn_blocking(move || {
        VerifiedPackage::extract_and_verify(stage, &archive, release, &child)
    })
    .await
    .map_err(|_| "升级解压任务失败。".to_owned())?
}

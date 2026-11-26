use std::fs::OpenOptions;
use std::path::Path;
use std::time::Duration;

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

pub use cst_updater::verify::{Stage, VerifiedPackage, verify_tree};

use crate::update::manifest::VerifiedRelease;
use crate::update::network::{Fetcher, UpdateError};

fn stopped(cancel: &CancellationToken) -> Result<(), UpdateError> {
    if cancel.is_cancelled() {
        Err(UpdateError::Cancelled)
    } else {
        Ok(())
    }
}

pub(crate) async fn prepare_with_progress<F>(
    root: &Path,
    release: VerifiedRelease,
    fetcher: &dyn Fetcher,
    cancel: CancellationToken,
    mut on_progress: F,
) -> Result<VerifiedPackage, UpdateError>
where
    F: FnMut(u64) + Send,
{
    stopped(&cancel)?;
    let stage = Stage::create(root, &release).map_err(|err| {
        if err.contains("剩余空间不足") {
            UpdateError::InsufficientDiskSpace
        } else {
            UpdateError::Other
        }
    })?;
    let pin = stage
        .base
        .pin_directory(&stage.relative, false)
        .map_err(|_| UpdateError::Other)?;
    let archive = stage.archive_path();
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&archive)
        .map_err(|_| UpdateError::Other)?;
    let mut file = tokio::fs::File::from_std(file);
    let mut count = 0u64;
    let download = async {
        let mut stream = fetcher
            .fetch(
                release.info.archive.url.clone(),
                release.info.archive.size,
                cancel.clone(),
            )
            .await?;
        let mut hash = Sha256::new();
        loop {
            stopped(&cancel)?;
            let next = tokio::time::timeout(Duration::from_secs(30), stream.body.next())
                .await
                .map_err(|_| UpdateError::Timeout {
                    phase: "download_chunk",
                    received_bytes: count,
                })?;
            let Some(chunk) = next else {
                break;
            };
            let chunk = chunk.map_err(|err| {
                if err.contains("超时") {
                    UpdateError::Timeout {
                        phase: "download_chunk",
                        received_bytes: count,
                    }
                } else if err.contains("超过大小限制") {
                    UpdateError::VerificationFailed {
                        stage: "archive_size",
                    }
                } else {
                    UpdateError::OfflineOrDns
                }
            })?;
            count = count
                .checked_add(chunk.len() as u64)
                .filter(|n| *n <= release.info.archive.size)
                .ok_or(UpdateError::VerificationFailed {
                    stage: "archive_size",
                })?;
            file.write_all(&chunk)
                .await
                .map_err(|_| UpdateError::InsufficientDiskSpace)?;
            hash.update(&chunk);
            on_progress(count);
        }
        file.sync_all()
            .await
            .map_err(|_| UpdateError::InsufficientDiskSpace)?;
        if count != release.info.archive.size
            || format!("{:x}", hash.finalize()) != release.info.archive.sha256
        {
            return Err(UpdateError::VerificationFailed {
                stage: "archive_hash",
            });
        }
        Ok(())
    };
    let result: Result<(), UpdateError> = tokio::select! { biased;
        _ = cancel.cancelled() => Err(UpdateError::Cancelled),
        r = tokio::time::timeout(Duration::from_secs(600), download) => r.map_err(|_| UpdateError::Timeout {
            phase: "download_total",
            received_bytes: count,
        })?,
    };
    drop(file);
    result?;
    drop(pin);
    stage
        .write_manifest(&release, &cancel)
        .map_err(|_| UpdateError::Other)?;
    let child = cancel.child_token();
    let _cancel_on_drop = child.clone().drop_guard();
    tokio::task::spawn_blocking(move || {
        VerifiedPackage::extract_and_verify(stage, &archive, release, &child)
    })
    .await
    .map_err(|_| UpdateError::Other)?
    .map_err(|_| {
        if cancel.is_cancelled() {
            UpdateError::Cancelled
        } else {
            UpdateError::VerificationFailed {
                stage: "tree_verify",
            }
        }
    })
}

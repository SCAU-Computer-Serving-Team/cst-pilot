//! Signed release preparation and host update coordination.
//! Offline verification, transaction apply, rollback, and `cst-updater.exe` live in `cst-updater`.

pub mod activity;
pub mod handoff;
pub mod network;
pub mod service;
pub mod verify;

pub use cst_updater::{applier, layout, lock, manifest, process};
pub use network::UpdateError;

use std::{path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;

use manifest::{ReleaseKey, VerifiedRelease};
use network::{Fetcher, NativeFetcher};
use verify::VerifiedPackage;

#[derive(Clone)]
pub struct UpdateClient {
    key: ReleaseKey,
    source: String,
    fetcher: Arc<dyn Fetcher>,
}

impl UpdateClient {
    /// The key is trusted host configuration, never a field in the downloaded manifest.
    pub fn new(source: &str, key: ReleaseKey) -> Result<Self, String> {
        Self::with_fetcher(source, key, Arc::new(NativeFetcher::default()))
    }

    /// Injection is restricted to trusted hosts; it does not change manifest verification.
    pub fn with_fetcher(
        source: &str,
        key: ReleaseKey,
        fetcher: Arc<dyn Fetcher>,
    ) -> Result<Self, String> {
        network::release_url(source)?;
        Ok(Self {
            key,
            source: source.into(),
            fetcher,
        })
    }

    pub fn key(&self) -> &ReleaseKey {
        &self.key
    }

    fn ensure_same_origin(&self, archive_url: &str) -> Result<(), UpdateError> {
        let source =
            network::release_url(&self.source).map_err(|_| UpdateError::DisallowedAddress)?;
        let archive =
            network::release_url(archive_url).map_err(|_| UpdateError::DisallowedAddress)?;
        if archive.origin() != source.origin() {
            return Err(UpdateError::PortalOrCrossOriginRedirect { status: None });
        }
        Ok(())
    }

    pub(crate) async fn check_detailed(
        &self,
        current: &str,
        cancel: CancellationToken,
    ) -> Result<Option<VerifiedRelease>, UpdateError> {
        let body = network::read(
            self.fetcher.as_ref(),
            &self.source,
            manifest::MAX_MANIFEST_BYTES as u64,
            cancel,
        )
        .await?;
        let release = self.key.verify(&body).map_err(|_| {
            if network::looks_like_html_body(&body) {
                UpdateError::PortalOrCrossOriginRedirect { status: Some(200) }
            } else {
                UpdateError::VerificationFailed {
                    stage: "manifest_verify",
                }
            }
        })?;
        self.ensure_same_origin(&release.info().archive.url)?;
        release
            .available_from(current)
            .map(|available| available.then_some(release))
            .map_err(|err| {
                if err.contains("最低版本") {
                    UpdateError::BelowMinUpgradable
                } else {
                    UpdateError::VerificationFailed {
                        stage: "version_compare",
                    }
                }
            })
    }

    pub async fn check(
        &self,
        current: &str,
        cancel: CancellationToken,
    ) -> Result<Option<VerifiedRelease>, String> {
        self.check_detailed(current, cancel)
            .await
            .map_err(|err| err.ui_message(false))
    }

    pub(crate) async fn prepare_detailed<F>(
        &self,
        root: &Path,
        release: &VerifiedRelease,
        cancel: CancellationToken,
        on_progress: F,
    ) -> Result<VerifiedPackage, UpdateError>
    where
        F: FnMut(u64) + Send,
    {
        // A release received from a different trusted client must still match this client's key.
        let checked =
            self.key
                .verify(release.envelope())
                .map_err(|_| UpdateError::VerificationFailed {
                    stage: "manifest_reverify",
                })?;
        self.ensure_same_origin(&checked.info().archive.url)?;
        let current = layout::read_version(root).map_err(|_| UpdateError::Other)?;
        let available = checked.available_from(&current).map_err(|err| {
            if err.contains("最低版本") {
                UpdateError::BelowMinUpgradable
            } else {
                UpdateError::VerificationFailed {
                    stage: "version_compare",
                }
            }
        })?;
        if !available {
            return Err(UpdateError::VerificationFailed {
                stage: "version_not_newer",
            });
        }
        verify::prepare_with_progress(root, checked, self.fetcher.as_ref(), cancel, on_progress)
            .await
    }

    pub async fn prepare(
        &self,
        root: &Path,
        release: &VerifiedRelease,
        cancel: CancellationToken,
    ) -> Result<VerifiedPackage, String> {
        self.prepare_detailed(root, release, cancel, |_| {})
            .await
            .map_err(|err| err.ui_message(true))
    }
}

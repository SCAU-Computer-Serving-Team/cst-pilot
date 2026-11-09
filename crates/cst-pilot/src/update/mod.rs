//! Signed release preparation. Applying a package is a separate, explicit operation.
pub mod activity;
pub mod applier;
pub mod handoff;
mod io;
mod json;
pub mod layout;
pub mod lock;
pub mod manifest;
pub mod network;
pub mod process;
pub mod service;
pub mod verify;

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
    pub fn compiled(oa_host: &str) -> Result<Self, String> {
        let host = network::release_url(oa_host)?;
        let source = host
            .join("/api/agent/releases/latest")
            .map_err(|_| "升级源无效。")?;
        Self::new(source.as_str(), ReleaseKey::compiled()?)
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
    pub async fn check(
        &self,
        current: &str,
        cancel: CancellationToken,
    ) -> Result<Option<VerifiedRelease>, String> {
        let body = network::read(
            self.fetcher.as_ref(),
            &self.source,
            manifest::MAX_MANIFEST_BYTES as u64,
            cancel,
        )
        .await?;
        let release = self.key.verify(&body)?;
        release
            .available_from(current)
            .map(|available| available.then_some(release))
    }
    pub async fn prepare(
        &self,
        root: &Path,
        release: &VerifiedRelease,
        cancel: CancellationToken,
    ) -> Result<VerifiedPackage, String> {
        // A release received from a different trusted client must still match this client's key.
        let checked = self.key.verify(release.envelope())?;
        if !checked.available_from(&layout::read_version(root)?)? {
            return Err("目标版本未高于当前工具包版本。".into());
        }
        verify::prepare(root, checked, self.fetcher.as_ref(), cancel).await
    }
}

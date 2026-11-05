use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pi_ai::{BoxFuture, ModelsStore, ModelsStoreEntry};

use crate::auth_storage::{
    AtomicWriteOutcome, DedicatedFileLockGuard, atomic_write_file_sync, strip_utf8_bom,
};

/// Persistent provider catalog cache. Updates merge under the same process lock.
#[derive(Clone)]
pub struct FileModelsStore {
    path: PathBuf,
}

impl FileModelsStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn load(path: &Path) -> Result<BTreeMap<String, ModelsStoreEntry>, String> {
        match std::fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str(strip_utf8_bom(&raw))
                .map_err(|_| "Invalid model catalog cache".to_owned()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(_) => Err("Cannot read model catalog cache".to_owned()),
        }
    }

    fn update(path: &Path, provider: &str, entry: Option<ModelsStoreEntry>) -> Result<(), String> {
        let _lock = DedicatedFileLockGuard::acquire(path)?;
        let mut entries = Self::load(path)?;
        if let Some(entry) = entry {
            entries.insert(provider.to_owned(), entry);
        } else {
            entries.remove(provider);
        }
        let bytes =
            serde_json::to_vec(&entries).map_err(|_| "Cannot serialize model cache".to_owned())?;
        match atomic_write_file_sync(path, &bytes, 0o600)? {
            AtomicWriteOutcome::CommittedAndSynced => Ok(()),
            AtomicWriteOutcome::CommittedWithParentSyncWarning(_) => {
                Err("Model cache committed but directory sync failed".to_owned())
            }
        }
    }
}

impl ModelsStore for FileModelsStore {
    fn read<'a>(
        &'a self,
        provider: &'a str,
    ) -> BoxFuture<'a, Result<Option<ModelsStoreEntry>, String>> {
        Box::pin(async move {
            let path = self.path.clone();
            let provider = provider.to_owned();
            tokio::task::spawn_blocking(move || Ok(Self::load(&path)?.remove(&provider)))
                .await
                .map_err(|_| "Model cache task failed".to_owned())?
        })
    }
    fn write<'a>(
        &'a self,
        provider: &'a str,
        entry: ModelsStoreEntry,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let path = self.path.clone();
            let provider = provider.to_owned();
            tokio::task::spawn_blocking(move || Self::update(&path, &provider, Some(entry)))
                .await
                .map_err(|_| "Model cache task failed".to_owned())?
        })
    }
    fn delete<'a>(&'a self, provider: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let path = self.path.clone();
            let provider = provider.to_owned();
            tokio::task::spawn_blocking(move || Self::update(&path, &provider, None))
                .await
                .map_err(|_| "Model cache task failed".to_owned())?
        })
    }
}

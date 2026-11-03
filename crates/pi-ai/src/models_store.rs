use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::auth::BoxFuture;
use crate::types::AnyModel;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsStoreEntry {
    pub models: Vec<AnyModel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
}

pub trait ModelsStore: Send + Sync {
    fn read<'a>(
        &'a self,
        provider_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<ModelsStoreEntry>, String>>;

    fn write<'a>(
        &'a self,
        provider_id: &'a str,
        entry: ModelsStoreEntry,
    ) -> BoxFuture<'a, Result<(), String>>;

    fn delete<'a>(&'a self, provider_id: &'a str) -> BoxFuture<'a, Result<(), String>>;
}

#[derive(Clone, Default)]
pub struct InMemoryModelsStore {
    entries: Arc<Mutex<BTreeMap<String, ModelsStoreEntry>>>,
}

impl InMemoryModelsStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ModelsStore for InMemoryModelsStore {
    fn read<'a>(
        &'a self,
        provider_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<ModelsStoreEntry>, String>> {
        Box::pin(async move {
            let guard = self.entries.lock().await;
            Ok(guard.get(provider_id).cloned())
        })
    }

    fn write<'a>(
        &'a self,
        provider_id: &'a str,
        entry: ModelsStoreEntry,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let mut guard = self.entries.lock().await;
            guard.insert(provider_id.to_owned(), entry);
            Ok(())
        })
    }

    fn delete<'a>(&'a self, provider_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let mut guard = self.entries.lock().await;
            guard.remove(provider_id);
            Ok(())
        })
    }
}

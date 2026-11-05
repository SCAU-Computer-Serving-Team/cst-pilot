mod callback;
mod discovery;
mod flow;
pub use callback::{McpOAuthCallback, McpOAuthCallbackServer};
pub use discovery::{
    McpOAuthChallenge, McpOAuthMetadata, discover_mcp_oauth, parse_oauth_challenge,
};
pub use flow::{McpOAuthRedirectFn, McpOAuthSession, generate_mcp_pkce};

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::auth_storage::{
    AtomicWriteOutcome, DedicatedFileLockGuard, atomic_write_file_sync, strip_utf8_bom,
};

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpOAuthCredential {
    pub access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<McpOAuthMetadata>,
}

impl std::fmt::Debug for McpOAuthCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpOAuthCredential")
            .field("access_token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct McpOAuthStore {
    file_path: PathBuf,
}

impl McpOAuthStore {
    pub fn new(agent_dir: &Path) -> Self {
        Self {
            file_path: agent_dir.join("mcp-auth.json"),
        }
    }

    pub fn load_all(&self) -> BTreeMap<String, McpOAuthCredential> {
        self.try_load_all().unwrap_or_default()
    }

    pub fn try_load_all(&self) -> Result<BTreeMap<String, McpOAuthCredential>, String> {
        let raw = match fs::read_to_string(&self.file_path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new());
            }
            Err(_) => return Err("Failed to read mcp-auth.json".to_owned()),
        };
        serde_json::from_str(strip_utf8_bom(&raw))
            .map_err(|_| "Invalid mcp-auth.json; existing credentials were preserved".to_owned())
    }

    pub fn get(&self, server: &str) -> Option<McpOAuthCredential> {
        self.load_all().remove(server)
    }

    pub fn set(&self, server: &str, credential: McpOAuthCredential) -> Result<(), String> {
        let _lock = DedicatedFileLockGuard::acquire(&self.file_path)?;
        let mut all = self.try_load_all()?;
        all.insert(server.to_owned(), credential);
        self.save_all(&all)
    }

    pub fn remove(&self, server: &str) -> Result<bool, String> {
        let _lock = DedicatedFileLockGuard::acquire(&self.file_path)?;
        let mut all = self.try_load_all()?;
        let existed = all.remove(server).is_some();
        if existed {
            self.save_all(&all)?;
        }
        Ok(existed)
    }

    pub(super) async fn transact<F, Fut, T>(&self, server: String, update: F) -> Result<T, String>
    where
        F: FnOnce(Option<McpOAuthCredential>) -> Fut + Send,
        Fut: std::future::Future<Output = Result<(Option<McpOAuthCredential>, T), String>> + Send,
        T: Send,
    {
        let _lock = DedicatedFileLockGuard::acquire_async(&self.file_path).await?;
        let store = self.clone();
        let mut all = tokio::task::spawn_blocking(move || store.try_load_all())
            .await
            .map_err(|_| "MCP credential read task failed")??;
        let (credential, result) = update(all.remove(&server)).await?;
        if let Some(credential) = credential {
            all.insert(server, credential);
        }
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.save_all(&all))
            .await
            .map_err(|_| "MCP credential write task failed")??;
        Ok(result)
    }

    fn save_all(&self, all: &BTreeMap<String, McpOAuthCredential>) -> Result<(), String> {
        let body = serde_json::to_string_pretty(all).map_err(|e| e.to_string())?;
        match atomic_write_file_sync(&self.file_path, format!("{body}\n").as_bytes(), 0o600)? {
            AtomicWriteOutcome::CommittedAndSynced => Ok(()),
            AtomicWriteOutcome::CommittedWithParentSyncWarning(warning) => Err(warning),
        }
    }
}

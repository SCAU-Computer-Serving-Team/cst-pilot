use std::collections::BTreeMap;
use std::path::Path;

use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use ed25519_dalek::{Signer, SigningKey};
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::runbook::storage::Storage;

/// Local secret has no Debug/Serialize implementation and never enters records.
#[derive(Clone)]
pub struct Installation {
    pub installation_id: String,
    key: Zeroizing<[u8; 32]>,
}
impl Installation {
    /// No ephemeral identity on read-only media or damage. Existing bytes stay unchanged.
    pub fn load(root: &Path) -> Option<Self> {
        let storage = Storage::new(root).ok()?;
        let _paths = storage
            .pin_directory(Path::new("agent/home/telemetry"), true)
            .ok()?;
        let path = storage.root.join("agent/home/telemetry/identity.json");
        match storage.read_limited(&path, 4096) {
            Ok(Some(bytes)) => return Self::parse(&bytes),
            Ok(None) => (),
            Err(_) => return None,
        }
        let owner = super::owner::Owner::global().ok()?;
        let cancel = CancellationToken::new();
        let _transaction =
            super::file_lock::FileLock::acquire(&storage.root, &path, &owner.proof(), &cancel)
                .ok()?;
        match storage.read_limited(&path, 4096) {
            Ok(Some(bytes)) => return Self::parse(&bytes),
            Ok(None) => (),
            Err(_) => return None,
        }
        let mut key = Zeroizing::new([0; 32]);
        rand::fill(key.as_mut());
        let value = Self {
            installation_id: Uuid::new_v4().to_string(),
            key,
        };
        let encoded = Zeroizing::new(json!({"installationId":value.installation_id,"identityKey":STANDARD.encode(value.key.as_ref())}).to_string().into_bytes());
        if storage
            .atomic(&path, &encoded, false, &CancellationToken::new())
            .is_ok()
        {
            return Some(value);
        }
        // A competing initializer may have published first; never overwrite it.
        storage
            .read_limited(&path, 4096)
            .ok()
            .flatten()
            .and_then(|bytes| Self::parse(&bytes))
    }
    fn parse(bytes: &[u8]) -> Option<Self> {
        let raw: serde_json::Value = serde_json::from_slice(bytes).ok()?;
        let id = raw.get("installationId")?.as_str()?;
        if Uuid::parse_str(id).ok()?.to_string() != id {
            return None;
        }
        let bytes = Zeroizing::new(STANDARD.decode(raw.get("identityKey")?.as_str()?).ok()?);
        let key: [u8; 32] = bytes.as_slice().try_into().ok()?;
        Some(Self {
            installation_id: id.into(),
            key: Zeroizing::new(key),
        })
    }
    pub fn environment_id(&self, guid: &str) -> String {
        let text = json!(["environment", guid.trim().to_lowercase()]).to_string();
        hex(&self.hmac(text.as_bytes()))
    }
    fn hmac(&self, message: &[u8]) -> [u8; 32] {
        let mut hmac = Hmac::<Sha256>::new_from_slice(self.key.as_ref()).expect("fixed key length");
        hmac.update(message);
        hmac.finalize().into_bytes().into()
    }
    /// Signs exact body bytes; caller may replace Authorization with a valid OAuth Bearer.
    pub fn headers(&self, body: &[u8], now_ms: u64) -> BTreeMap<String, String> {
        let seed = Zeroizing::new(self.hmac(b"upload-signing"));
        let signing = SigningKey::from_bytes(&seed);
        // RFC 8410 SubjectPublicKeyInfo for id-Ed25519, matching Node's DER/SPKI output.
        let mut public = vec![
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
        ];
        public.extend_from_slice(signing.verifying_key().as_bytes());
        let timestamp = now_ms.to_string();
        let message = format!(
            "telemetry-upload\n{}\n{timestamp}\n{}",
            self.installation_id,
            hex(&Sha256::digest(body))
        );
        BTreeMap::from([
            (
                "Authorization".into(),
                format!("Installation {}", self.installation_id),
            ),
            (
                "X-Telemetry-Installation".into(),
                self.installation_id.clone(),
            ),
            ("X-Telemetry-Key".into(), URL_SAFE_NO_PAD.encode(public)),
            ("X-Telemetry-Time".into(), timestamp),
            (
                "X-Telemetry-Signature".into(),
                URL_SAFE_NO_PAD.encode(signing.sign(message.as_bytes()).to_bytes()),
            ),
        ])
    }
}
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

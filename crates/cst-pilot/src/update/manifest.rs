use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use super::{layout, network};

pub const MAX_MANIFEST_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_ARCHIVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_UNPACKED_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_FILES: usize = 100_000;
pub const MANIFEST_DOMAIN: &[u8] = b"cst-pilot-release-manifest-v1\n";
pub const SUMS_DOMAIN: &[u8] = b"cst-pilot-release-sums-v1\n";

#[derive(Clone)]
pub struct ReleaseKey(VerifyingKey);
impl ReleaseKey {
    pub fn from_bytes(bytes: &[u8; 32]) -> Result<Self, String> {
        let key = VerifyingKey::from_bytes(bytes).map_err(|_| "升级签名公钥无效。")?;
        if key.is_weak() {
            return Err("升级签名公钥无效。".into());
        }
        Ok(Self(key))
    }
    pub fn compiled() -> Result<Self, String> {
        let raw = option_env!("CST_UPDATE_PUBLIC_KEY").ok_or("当前构建未配置官方升级公钥。")?;
        let bytes = STANDARD.decode(raw).map_err(|_| "构建中的升级公钥无效。")?;
        Self::from_bytes(
            bytes
                .as_slice()
                .try_into()
                .map_err(|_| "构建中的升级公钥无效。")?,
        )
    }
    fn signature(&self, domain: &[u8], data: &[u8], encoded: &str) -> Result<(), String> {
        if encoded.len() > 128 {
            return Err("升级签名无效。".into());
        }
        let bytes = STANDARD.decode(encoded).map_err(|_| "升级签名无效。")?;
        let signature = Signature::from_slice(&bytes).map_err(|_| "升级签名无效。")?;
        let mut text = Vec::with_capacity(domain.len() + data.len());
        text.extend_from_slice(domain);
        text.extend_from_slice(data);
        self.0
            .verify_strict(&text, &signature)
            .map_err(|_| "升级签名验证失败。".into())
    }
    pub fn verify(&self, envelope: &[u8]) -> Result<VerifiedRelease, String> {
        if envelope.len() > MAX_MANIFEST_BYTES {
            return Err("升级清单超过大小限制。".into());
        }
        let signed: SignedManifest =
            serde_json::from_slice(envelope).map_err(|_| "升级清单格式无效。")?;
        self.signature(
            MANIFEST_DOMAIN,
            signed.payload.as_bytes(),
            &signed.signature,
        )?;
        let info: ReleaseManifest =
            serde_json::from_str(&signed.payload).map_err(|_| "升级清单字段无效。")?;
        if info.schema_version != 1
            || info.platform != "windows-x86_64"
            || info.release_notes.len() > 16 * 1024
            || info.sha256_sums.len() > 6 * 1024 * 1024
            || info.archive.size == 0
            || info.archive.size > MAX_ARCHIVE_BYTES
            || info.archive.unpacked_size == 0
            || info.archive.unpacked_size > MAX_UNPACKED_BYTES
            || !hash_text(&info.archive.sha256)
        {
            return Err("升级清单不受支持或超过资源限制。".into());
        }
        version(&info.version)?;
        if version(&info.min_upgradable_version)? > version(&info.version)? {
            return Err("升级最低版本无效。".into());
        }
        chrono::DateTime::parse_from_rfc3339(&info.built_at).map_err(|_| "升级构建时间无效。")?;
        network::release_url(&info.archive.url)?;
        self.signature(
            SUMS_DOMAIN,
            info.sha256_sums.as_bytes(),
            &info.sha256_signature,
        )?;
        let sums = parse_sums(&info.sha256_sums)?;
        Ok(VerifiedRelease {
            info,
            sums,
            envelope: envelope.to_vec(),
        })
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedManifest {
    /// Exact UTF-8 JSON text; signature does not depend on object key reordering.
    pub payload: String,
    pub signature: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseManifest {
    pub schema_version: u32,
    pub platform: String,
    pub version: String,
    pub built_at: String,
    pub min_upgradable_version: String,
    pub release_notes: String,
    pub archive: Archive,
    pub sha256_sums: String,
    pub sha256_signature: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Archive {
    pub url: String,
    pub size: u64,
    pub unpacked_size: u64,
    pub sha256: String,
}
#[derive(Clone)]
pub struct VerifiedRelease {
    pub(crate) info: ReleaseManifest,
    pub(crate) sums: BTreeMap<String, String>,
    envelope: Vec<u8>,
}
impl VerifiedRelease {
    pub fn info(&self) -> &ReleaseManifest {
        &self.info
    }
    pub fn envelope(&self) -> &[u8] {
        &self.envelope
    }
    pub fn available_from(&self, current: &str) -> Result<bool, String> {
        let current = version(current)?;
        if version(&self.info.version)? <= current {
            return Ok(false);
        }
        if current < version(&self.info.min_upgradable_version)? {
            return Err("当前版本低于可原位升级的最低版本，请下载完整发行包。".into());
        }
        Ok(true)
    }
}
pub fn version(text: &str) -> Result<Version, String> {
    if text.len() > 64 || text.trim() != text {
        return Err("工具包版本格式无效。".into());
    }
    Version::parse(text.strip_prefix('v').unwrap_or(text))
        .map_err(|_| "工具包版本格式无效。".into())
}
fn hash_text(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(crate) fn parse_sums(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut sums: BTreeMap<String, String> = BTreeMap::new();
    let mut names = BTreeSet::new();
    for line in text.lines() {
        let (hash, path) = line.split_once("  ").ok_or("文件校验清单格式无效。")?;
        if !hash_text(hash) || path == "SHA256SUMS" {
            return Err("文件校验清单格式无效。".into());
        }
        layout::classify(path)?;
        if !names.insert(path.to_lowercase()) || sums.insert(path.into(), hash.into()).is_some() {
            return Err("文件校验清单存在重复或大小写冲突。".into());
        }
        if sums.len() > MAX_FILES {
            return Err("升级包文件数量超过限制。".into());
        }
    }
    for path in sums.keys() {
        let mut prefix = String::new();
        let parts: Vec<_> = path.split('/').collect();
        for part in &parts[..parts.len() - 1] {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            if names.contains(&prefix.to_lowercase()) {
                return Err("升级清单存在文件与目录冲突。".into());
            }
        }
    }
    if sums.is_empty() {
        return Err("文件校验清单为空。".into());
    }
    Ok(sums)
}

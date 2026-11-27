use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::{Host, Url};

use crate::layout;

pub const MAX_MANIFEST_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_ARCHIVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_UNPACKED_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_FILES: usize = 100_000;
pub const MANIFEST_DOMAIN: &[u8] = b"cst-pilot-release-manifest-v1\n";
pub const SUMS_DOMAIN: &[u8] = b"cst-pilot-release-sums-v1\n";

/// Built-in official stable update manifest endpoint (`https://www.cstoa.top`).
pub const OFFICIAL_MANIFEST_URL: &str = "https://www.cstoa.top/api/agent/releases/latest";
/// Built-in official beta update manifest endpoint (`https://www.cstoa.top`).
pub const OFFICIAL_BETA_MANIFEST_URL: &str = "https://www.cstoa.top/api/agent/releases/beta";
/// Built-in official 32-byte Ed25519 release verifying public key (44-char canonical Base64).
pub const OFFICIAL_PUBLIC_KEY_B64: &str = "DWWitg4ipNgyWT/Yvpukel2sxQzP9plFgnv++do188E=";
/// Expected 16-char lowercase hex SHA-256 prefix of [`OFFICIAL_PUBLIC_KEY_B64`].
pub const OFFICIAL_PUBLIC_KEY_FINGERPRINT: &str = "146afb5bf759af97";

/// Delimited marker prefix embedded into compiled binaries for zero-execution static inspection.
pub const UPDATE_ROOT_MARKER_PREFIX: &[u8] = b"CST_UPDATE_ROOT_V1\n";
/// Maximum byte length of a compiled-in manifest URL embedded in [`COMPILED_UPDATE_ROOT_MARKER`].
pub const MAX_COMPILED_MANIFEST_URL_BYTES: usize = 200;
/// Maximum byte length of a compiled-in public key string embedded in [`COMPILED_UPDATE_ROOT_MARKER`].
pub const MAX_COMPILED_PUBLIC_KEY_BYTES: usize = 88;

const COMPILED_MANIFEST_URL_RAW: &str = match option_env!("CST_UPDATE_MANIFEST_URL") {
    Some(v) => v,
    None => OFFICIAL_MANIFEST_URL,
};

const COMPILED_PUBLIC_KEY_RAW: &str = match option_env!("CST_UPDATE_PUBLIC_KEY") {
    Some(v) => v,
    None => OFFICIAL_PUBLIC_KEY_B64,
};

const fn trim_ascii_bytes(bytes: &[u8]) -> &[u8] {
    let mut start = 0usize;
    let mut end = bytes.len();
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let mut out_len = end - start;
    let mut slice = bytes;
    let mut s = 0usize;
    while s < start {
        if let Some((_, rest)) = slice.split_first() {
            slice = rest;
        }
        s += 1;
    }
    while slice.len() > out_len {
        if let Some((_, head)) = slice.split_last() {
            slice = head;
        } else {
            out_len = 0;
        }
    }
    slice
}

const fn is_disabled_bytes(trimmed: &[u8]) -> bool {
    matches!(
        trimmed.len(),
        8 if trimmed[0].eq_ignore_ascii_case(&b'd')
            && trimmed[1].eq_ignore_ascii_case(&b'i')
            && trimmed[2].eq_ignore_ascii_case(&b's')
            && trimmed[3].eq_ignore_ascii_case(&b'a')
            && trimmed[4].eq_ignore_ascii_case(&b'b')
            && trimmed[5].eq_ignore_ascii_case(&b'l')
            && trimmed[6].eq_ignore_ascii_case(&b'e')
            && trimmed[7].eq_ignore_ascii_case(&b'd')
    ) || matches!(
        trimmed.len(),
        4 if trimmed[0].eq_ignore_ascii_case(&b'n')
            && trimmed[1].eq_ignore_ascii_case(&b'o')
            && trimmed[2].eq_ignore_ascii_case(&b'n')
            && trimmed[3].eq_ignore_ascii_case(&b'e')
    ) || matches!(
        trimmed.len(),
        3 if trimmed[0].eq_ignore_ascii_case(&b'o')
            && trimmed[1].eq_ignore_ascii_case(&b'f')
            && trimmed[2].eq_ignore_ascii_case(&b'f')
    )
}

const fn assert_printable_ascii_token(bytes: &[u8]) {
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b <= 0x20 || b >= 0x7f {
            panic!(
                "CST_UPDATE_MANIFEST_URL / CST_UPDATE_PUBLIC_KEY must not contain whitespace or control bytes"
            );
        }
        i += 1;
    }
}

const fn build_update_root_marker(url: &str, key: &str) -> [u8; 320] {
    let mut out = [0u8; 320];
    let prefix = UPDATE_ROOT_MARKER_PREFIX;
    let u_trim = trim_ascii_bytes(url.as_bytes());
    let k_trim = trim_ascii_bytes(key.as_bytes());
    let disabled = is_disabled_bytes(u_trim) || is_disabled_bytes(k_trim);
    let url_b: &[u8] = if disabled {
        b"disabled"
    } else if u_trim.is_empty() {
        OFFICIAL_MANIFEST_URL.as_bytes()
    } else {
        u_trim
    };
    let key_b: &[u8] = if disabled {
        b"disabled"
    } else if k_trim.is_empty() {
        OFFICIAL_PUBLIC_KEY_B64.as_bytes()
    } else {
        k_trim
    };
    if url_b.len() > MAX_COMPILED_MANIFEST_URL_BYTES || key_b.len() > MAX_COMPILED_PUBLIC_KEY_BYTES
    {
        panic!("CST_UPDATE_MANIFEST_URL / CST_UPDATE_PUBLIC_KEY exceeds static marker capacity");
    }
    assert_printable_ascii_token(url_b);
    assert_printable_ascii_token(key_b);
    let mut idx = 0;
    let mut i = 0;
    while i < prefix.len() {
        out[idx] = prefix[i];
        idx += 1;
        i += 1;
    }
    i = 0;
    while i < url_b.len() {
        out[idx] = url_b[i];
        idx += 1;
        i += 1;
    }
    out[idx] = b'\n';
    idx += 1;
    i = 0;
    while i < key_b.len() {
        out[idx] = key_b[i];
        idx += 1;
        i += 1;
    }
    out[idx] = b'\n';
    out
}

#[used]
static COMPILED_UPDATE_ROOT_MARKER: [u8; 320] =
    build_update_root_marker(COMPILED_MANIFEST_URL_RAW, COMPILED_PUBLIC_KEY_RAW);

/// Formats the exact byte sequence embedded in `.rdata` for a `(manifest_url, public_key_b64)` pair.
pub fn format_update_root_marker(manifest_url: &str, public_key_b64: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(
        UPDATE_ROOT_MARKER_PREFIX.len() + manifest_url.len() + public_key_b64.len() + 2,
    );
    v.extend_from_slice(UPDATE_ROOT_MARKER_PREFIX);
    v.extend_from_slice(manifest_url.as_bytes());
    v.push(b'\n');
    v.extend_from_slice(public_key_b64.as_bytes());
    v.push(b'\n');
    v
}

/// Returns `true` when a compile-time update configuration value explicitly disables online updates.
pub fn is_disabled_update_sentinel(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "disabled" | "none" | "off"
    )
}

/// Pure offline HTTPS release URL validation shared by `cst-updater` and `cst-kit`.
pub fn validate_release_url(text: &str) -> Result<Url, String> {
    if text.is_empty() || text.len() > 2048 || !text.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err("URL 包含空白、控制字符或长度超出限制。".into());
    }
    let url = Url::parse(text).map_err(|_| "URL 无效。")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("仅允许不含登录凭据的 HTTP 或 HTTPS 地址。".into());
    }
    if let Some(host) = url.host_str() {
        let host = host.trim_end_matches('.').to_lowercase();
        if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
            return Err("网络策略禁止访问本机或内网地址。".into());
        }
    }
    match url.host() {
        Some(Host::Ipv4(ip)) => validate_public_ip(IpAddr::V4(ip))?,
        Some(Host::Ipv6(ip)) => validate_public_ip(IpAddr::V6(ip))?,
        _ => {}
    }
    if url.scheme() != "https" || url.fragment().is_some() {
        return Err("升级源必须使用不含片段的 HTTPS 地址。".into());
    }
    Ok(url)
}

fn validate_public_ip(ip: IpAddr) -> Result<(), String> {
    let allowed = match ip {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                public_v4(v4)
            } else {
                public_v6(ip)
            }
        }
    };
    if allowed {
        Ok(())
    } else {
        Err("网络策略禁止访问本机或内网地址。".into())
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || a == 10
        || a == 127
        || a >= 224
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 192 && b == 0 && (c == 0 || c == 2))
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113))
}

fn public_v6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    (segments[0] & 0xe000) == 0x2000
        && !(segments[0] == 0x2001 && segments[1] < 0x0200)
        && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
        && segments[0] != 0x2002
        && !(segments[0] == 0x3fff && (segments[1] & 0xf000) == 0)
}

#[derive(Clone, PartialEq, Eq)]
pub struct ReleaseKey(VerifyingKey);

impl ReleaseKey {
    pub fn from_bytes(bytes: &[u8; 32]) -> Result<Self, String> {
        let key = VerifyingKey::from_bytes(bytes).map_err(|_| "升级签名公钥无效。")?;
        if key.is_weak() {
            return Err("升级签名公钥无效。".into());
        }
        Ok(Self(key))
    }

    pub fn official() -> Result<Self, String> {
        let bytes = STANDARD
            .decode(OFFICIAL_PUBLIC_KEY_B64)
            .map_err(|_| "内置官方升级公钥无效。".to_owned())?;
        Self::from_bytes(
            bytes
                .as_slice()
                .try_into()
                .map_err(|_| "内置官方升级公钥无效。".to_owned())?,
        )
    }

    /// Resolves the verifying key from compile-time environment values, falling back to
    /// [`OFFICIAL_PUBLIC_KEY_B64`] when unset, and failing closed when either variable is a
    /// disabled sentinel (`disabled` / `none` / `off`).
    pub fn compiled_from_opt(
        manifest_url_env: Option<&str>,
        public_key_env: Option<&str>,
    ) -> Result<Self, String> {
        if manifest_url_env.is_some_and(is_disabled_update_sentinel)
            || public_key_env.is_some_and(is_disabled_update_sentinel)
        {
            return Err("当前构建未启用在线更新。".into());
        }
        if let Some(raw) = public_key_env.map(str::trim).filter(|s| !s.is_empty()) {
            if raw.len() > MAX_COMPILED_PUBLIC_KEY_BYTES
                || !raw.bytes().all(|b| (0x21..=0x7e).contains(&b))
            {
                return Err("构建中的升级公钥长度或字符无效。".into());
            }
            let bytes = STANDARD.decode(raw).map_err(|_| "构建中的升级公钥无效。")?;
            if STANDARD.encode(&bytes) != raw {
                return Err("构建中的升级公钥必须为标准 Base64 编码。".into());
            }
            return Self::from_bytes(
                bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| "构建中的升级公钥无效。")?,
            );
        }
        Self::official()
    }

    pub fn compiled() -> Result<Self, String> {
        let _ = std::hint::black_box(&COMPILED_UPDATE_ROOT_MARKER);
        Self::compiled_from_opt(
            option_env!("CST_UPDATE_MANIFEST_URL"),
            option_env!("CST_UPDATE_PUBLIC_KEY"),
        )
    }

    /// Returns the 16-char lowercase hex SHA-256 prefix of the 32-byte Ed25519 verifying key.
    pub fn fingerprint(&self) -> String {
        let hex = format!("{:x}", Sha256::digest(self.0.as_bytes()));
        hex[..16].to_owned()
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
        validate_release_url(&info.archive.url)?;
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
    pub info: ReleaseManifest,
    pub sums: BTreeMap<String, String>,
    envelope: Vec<u8>,
}

impl VerifiedRelease {
    pub fn info(&self) -> &ReleaseManifest {
        &self.info
    }

    pub fn sums(&self) -> &BTreeMap<String, String> {
        &self.sums
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
        if !hash_text(hash) || path == layout::SHA256SUMS_PATH {
            return Err("文件校验清单格式无效。".into());
        }
        if layout::classify_release(path)? != layout::Area::Program {
            return Err("文件校验清单不能包含用户数据路径。".into());
        }
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

/// Resolves the manifest URL from compile-time environment values, falling back to
/// [`OFFICIAL_MANIFEST_URL`] when unset, and failing closed when either variable is a
/// disabled sentinel (`disabled` / `none` / `off`).
pub fn resolve_compiled_manifest_url_from_opt(
    manifest_url_env: Option<&str>,
    public_key_env: Option<&str>,
) -> Result<Url, String> {
    if manifest_url_env.is_some_and(is_disabled_update_sentinel)
        || public_key_env.is_some_and(is_disabled_update_sentinel)
    {
        return Err("当前构建未启用在线更新。".into());
    }
    let effective = manifest_url_env
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(OFFICIAL_MANIFEST_URL);
    if effective.len() > MAX_COMPILED_MANIFEST_URL_BYTES {
        return Err("升级源地址超出编译期标记容量上限。".into());
    }
    let parsed = validate_release_url(effective)?;
    if parsed.as_str() != effective {
        return Err("升级源地址必须为已规范化的标准 HTTPS URL。".into());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("升级源地址不允许包含查询参数或片段。".into());
    }
    Ok(parsed)
}

pub fn compiled_manifest_url() -> Result<Url, String> {
    resolve_compiled_manifest_url_from_opt(
        option_env!("CST_UPDATE_MANIFEST_URL"),
        option_env!("CST_UPDATE_PUBLIC_KEY"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_trust_root_constants_are_canonical_and_match_fingerprint() {
        let key = ReleaseKey::official().expect("official release key must be valid");
        assert_eq!(key.fingerprint(), OFFICIAL_PUBLIC_KEY_FINGERPRINT);

        let default_key = ReleaseKey::compiled_from_opt(None, None).expect("default compiled key");
        assert_eq!(default_key.fingerprint(), OFFICIAL_PUBLIC_KEY_FINGERPRINT);

        let default_url =
            resolve_compiled_manifest_url_from_opt(None, None).expect("default compiled url");
        assert_eq!(default_url.as_str(), OFFICIAL_MANIFEST_URL);

        let beta_url =
            resolve_compiled_manifest_url_from_opt(Some(OFFICIAL_BETA_MANIFEST_URL), None)
                .expect("beta url override");
        assert_eq!(beta_url.as_str(), OFFICIAL_BETA_MANIFEST_URL);
    }

    #[test]
    fn disabled_sentinels_and_query_urls_fail_closed_symmetrically() {
        for sentinel in ["disabled", "DISABLED", " none ", "off"] {
            assert!(ReleaseKey::compiled_from_opt(Some(sentinel), None).is_err());
            assert!(ReleaseKey::compiled_from_opt(None, Some(sentinel)).is_err());
            assert!(resolve_compiled_manifest_url_from_opt(Some(sentinel), None).is_err());
            assert!(resolve_compiled_manifest_url_from_opt(None, Some(sentinel)).is_err());
        }
        assert!(
            resolve_compiled_manifest_url_from_opt(
                Some("https://www.cstoa.top/api/agent/releases/latest?ch=beta"),
                None,
            )
            .is_err()
        );
    }
}

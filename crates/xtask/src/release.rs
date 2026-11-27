use std::env;
use std::fs::{self, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use cst_updater::layout::{BUILD_INFO_PATH, SERVER_EXE_PATH, SHA256SUMS_PATH, UPDATER_EXE_PATH};
use cst_updater::manifest::{
    Archive, MANIFEST_DOMAIN, OFFICIAL_BETA_MANIFEST_URL, OFFICIAL_MANIFEST_URL,
    OFFICIAL_PUBLIC_KEY_B64, ReleaseKey, ReleaseManifest, SUMS_DOMAIN, SignedManifest,
    UPDATE_ROOT_MARKER_PREFIX, format_update_root_marker, is_disabled_update_sentinel,
    validate_release_url,
};
#[cfg(windows)]
use cst_updater::verify::VerifiedPackage;
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

pub const DEFAULT_PUBLIC_BASE_URL: &str = "https://www.cstoa.top/api/agent/releases";
pub const DEFAULT_ADMIN_URL: &str = "http://127.0.0.1:18787/v1/admin/releases";
pub const DEFAULT_SSH_REMOTE_PORT: u16 = 8787;
pub const ENV_MANIFEST_URL: &str = "CST_UPDATE_MANIFEST_URL";
pub const ENV_PUBLIC_KEY: &str = "CST_UPDATE_PUBLIC_KEY";
pub const ENV_PRIVATE_KEY_FILE: &str = "CST_RELEASE_PRIVATE_KEY_FILE";
pub const ENV_ADMIN_TOKEN: &str = "CST_RELEASE_ADMIN_TOKEN";
const MAX_SERVER_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SHA256SUMS_BYTES: u64 = 6 * 1024 * 1024;
const MAX_ADMIN_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_PUBLIC_MANIFEST_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DistChannel {
    Stable,
    Beta,
    Disabled,
}

impl DistChannel {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "stable" => Ok(Self::Stable),
            "beta" => Ok(Self::Beta),
            "disabled" | "none" | "off" => Ok(Self::Disabled),
            other => Err(format!(
                "不支持的 --channel：{other}（仅允许 stable、beta 或 disabled）。"
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateBuildConfig {
    pub manifest_url: String,
    pub public_key_b64: String,
    pub public_key_fingerprint: String,
}

/// Trusted build provenance emitted by `xtask dist` on the keyless build host after compiling
/// and executing `cst-updater.exe --key-fingerprint` + `cst-server.exe --key-fingerprint`.
/// Consumed offline by `xtask sign-release` without executing any untrusted binary under the
/// signing account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildAttestation {
    pub schema_version: u32,
    pub version: String,
    pub platform: String,
    pub built_at: String,
    pub git_commit: String,
    pub update_enabled: bool,
    pub manifest_url: String,
    pub update_public_key_fingerprint: String,
    pub verified_key_probe: bool,
    pub archive_filename: String,
    pub archive_size: u64,
    pub archive_sha256: String,
    pub unpacked_size: u64,
    pub sha256_sums_sha256: String,
    pub updater_exe_sha256: String,
    pub server_exe_sha256: String,
}

fn is_lower_hex_len(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Writes the keyless build attestation JSON and returns its exact 64-char SHA-256 digest
/// for out-of-band pinning via `sign-release --expected-attestation-sha256`.
pub fn write_build_attestation(
    path: &Path,
    attestation: &BuildAttestation,
) -> Result<String, String> {
    if !is_lower_hex_len(&attestation.git_commit, 40) {
        return Err("构建证明 gitCommit 必须为 40 位小写十六进制提交哈希。".into());
    }
    let parsed_url = validate_release_url(&attestation.manifest_url)?;
    if parsed_url.query().is_some() || parsed_url.fragment().is_some() {
        return Err("构建证明 manifestUrl 不允许包含查询参数或片段。".into());
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|e| format!("无法创建构建证明目录 {}：{e}", parent.display()))?;
    }
    let mut bytes =
        serde_json::to_vec_pretty(attestation).map_err(|_| "构建证明序列化失败。".to_owned())?;
    bytes.push(b'\n');
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    fs::write(path, &bytes).map_err(|e| format!("无法写入构建证明 {}：{e}", path.display()))?;
    Ok(sha256)
}

fn read_and_verify_build_attestation(
    path: &Path,
    expected_sha256: &str,
) -> Result<BuildAttestation, String> {
    let expected = expected_sha256.trim().to_ascii_lowercase();
    if !is_lower_hex_len(&expected, 64) {
        return Err("--expected-attestation-sha256 必须为 64 位小写十六进制 SHA-256 摘要。".into());
    }
    let bytes = fs::read(path).map_err(|e| format!("无法读取构建证明 {}：{e}", path.display()))?;
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if actual != expected {
        return Err(format!(
            "构建证明 SHA-256（{actual}）与命令行指定摘要（{expected}）不一致，拒绝信任该构建记录。"
        ));
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("构建证明 JSON 格式无效：{e}"))
}

pub fn parse_canonical_public_key(raw_b64: &str) -> Result<(ReleaseKey, [u8; 32], String), String> {
    let trimmed = raw_b64.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return Err("升级公钥不能为空且必须为 44 字符标准 Base64。".into());
    }
    let decoded = STANDARD
        .decode(trimmed)
        .map_err(|_| "升级公钥 Base64 解码失败。".to_owned())?;
    if STANDARD.encode(&decoded) != trimmed {
        return Err("升级公钥必须使用标准带填充 Base64 编码。".into());
    }
    let arr: [u8; 32] = decoded
        .as_slice()
        .try_into()
        .map_err(|_| "升级公钥必须为 32 字节 Ed25519 公钥。".to_owned())?;
    let key = ReleaseKey::from_bytes(&arr)?;
    let fp = public_key_fingerprint(&arr);
    Ok((key, arr, fp))
}

pub fn public_key_fingerprint(public_key_bytes: &[u8; 32]) -> String {
    let hex = format!("{:x}", Sha256::digest(public_key_bytes));
    hex[..16].to_owned()
}

#[cfg(any(windows, test))]
fn verify_probe_command(mut cmd: Command, label: &str, expected_fp: &str) -> Result<(), String> {
    let output = cmd
        .output()
        .map_err(|e| format!("无法执行 {label} --key-fingerprint：{e}"))?;
    if !output.status.success() {
        return Err(format!(
            "{label} 公钥自检失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let actual_fp = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if actual_fp != expected_fp {
        return Err(format!(
            "{label} 实际编译内嵌公钥指纹（{actual_fp}）与预期（{expected_fp}）不一致，拒绝继续。"
        ));
    }
    Ok(())
}

/// Executes packaged `app/cst-updater.exe --key-fingerprint` and `app/cst-server.exe --key-fingerprint`
/// to prove both binaries actually verify updates with the expected compiled-in Ed25519 key.
pub fn verify_binaries_key_fingerprint(dist_dir: &Path, expected_fp: &str) -> Result<(), String> {
    #[cfg(not(windows))]
    {
        let _ = (dist_dir, expected_fp);
        Err(
            "校验 windows-x86_64 发行二进制内嵌公钥必须在 Windows 环境运行 app/cst-updater.exe 与 app/cst-server.exe --key-fingerprint。"
                .into(),
        )
    }
    #[cfg(windows)]
    {
        for rel in [UPDATER_EXE_PATH, SERVER_EXE_PATH] {
            let mut cmd = isolated_probe_command(dist_dir, rel);
            cmd.arg("--key-fingerprint");
            verify_probe_command(cmd, rel, expected_fp)?;
        }
        Ok(())
    }
}

/// Verifies that a `--channel disabled` build genuinely compiled out online update capability in
/// both `app/cst-updater.exe` and `app/cst-server.exe` (`--key-fingerprint` must exit non-zero).
pub fn verify_binaries_update_disabled(dist_dir: &Path) -> Result<(), String> {
    #[cfg(not(windows))]
    {
        let _ = dist_dir;
        Ok(())
    }
    #[cfg(windows)]
    {
        for rel in [UPDATER_EXE_PATH, SERVER_EXE_PATH] {
            let mut cmd = isolated_probe_command(dist_dir, rel);
            let out = cmd
                .arg("--key-fingerprint")
                .output()
                .map_err(|e| format!("无法执行 {rel} --key-fingerprint：{e}"))?;
            if out.status.success() {
                return Err(format!(
                    "{rel} 在禁用在线更新构建下仍返回了升级公钥指纹，拒绝继续。"
                ));
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
fn isolated_probe_command(dist_dir: &Path, rel: &str) -> Command {
    let mut cmd = Command::new(dist_dir.join(rel));
    cmd.env_clear();
    for key in [
        "SystemRoot",
        "WINDIR",
        "ComSpec",
        "ProgramData",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "OS",
    ] {
        if let Some(value) = env::var_os(key) {
            cmd.env(key, value);
        }
    }
    cmd.current_dir(dist_dir);
    cmd
}

/// Pure-Rust static PE32+ (`x86_64`) header & `IMAGE_SUBSYSTEM_WINDOWS_GUI == 2` verification.
/// Never executes any instruction from the inspected binary.
pub fn verify_pe64_windows_gui_binary(bytes: &[u8], label: &str) -> Result<(), String> {
    if bytes.len() < 0x100 || &bytes[0..2] != b"MZ" {
        return Err(format!("{label} 不是有效的 Windows PE 可执行文件。"));
    }
    let pe_offset =
        u32::from_le_bytes([bytes[0x3c], bytes[0x3d], bytes[0x3e], bytes[0x3f]]) as usize;
    let Some(coff_offset) = pe_offset.checked_add(4) else {
        return Err(format!("{label} PE 头偏移溢出。"));
    };
    let Some(opt_offset) = coff_offset.checked_add(20) else {
        return Err(format!("{label} COFF 头偏移溢出。"));
    };
    if opt_offset.saturating_add(70) > bytes.len() || &bytes[pe_offset..coff_offset] != b"PE\0\0" {
        return Err(format!("{label} 缺少有效的 PE 签名头。"));
    }
    let machine = u16::from_le_bytes([bytes[coff_offset], bytes[coff_offset + 1]]);
    if machine != 0x8664 {
        return Err(format!(
            "{label} 目标架构不是 x86_64（Machine=0x{machine:04x}）。"
        ));
    }
    let opt_size = u16::from_le_bytes([bytes[coff_offset + 16], bytes[coff_offset + 17]]) as usize;
    if opt_size < 96 || opt_offset.saturating_add(opt_size) > bytes.len() {
        return Err(format!("{label} OptionalHeader 长度无效。"));
    }
    let magic = u16::from_le_bytes([bytes[opt_offset], bytes[opt_offset + 1]]);
    if magic != 0x020b {
        return Err(format!(
            "{label} 不是 64 位 PE32+ 可执行文件（Magic=0x{magic:04x}）。"
        ));
    }
    let subsystem = u16::from_le_bytes([bytes[opt_offset + 68], bytes[opt_offset + 69]]);
    if subsystem != 2 {
        return Err(format!(
            "{label} PE Subsystem 必须为 2（WINDOWS_GUI），实际为 {subsystem}。"
        ));
    }
    Ok(())
}

fn verify_exact_single_root_marker(
    bytes: &[u8],
    marker: &[u8],
    rel: &str,
    expected_manifest_url: &str,
) -> Result<(), String> {
    let prefix = UPDATE_ROOT_MARKER_PREFIX;
    if bytes.len() < marker.len() {
        return Err(format!("{rel} 体积过小，缺少编译期信任根标记。"));
    }
    let mut matched = 0usize;
    let mut pos = 0usize;
    while pos + prefix.len() <= bytes.len() {
        if &bytes[pos..pos + prefix.len()] == prefix {
            if pos + marker.len() > bytes.len() || &bytes[pos..pos + marker.len()] != marker {
                return Err(format!(
                    "{rel} 包含不匹配的编译期更新源或公钥静态标记（期望通道 {expected_manifest_url}），拒绝继续。"
                ));
            }
            matched += 1;
            pos += marker.len();
        } else {
            pos += 1;
        }
    }
    if matched == 0 {
        return Err(format!(
            "{rel} 未包含匹配的编译期更新源与公钥静态标记（期望通道 {expected_manifest_url}），拒绝继续。"
        ));
    }
    Ok(())
}

/// Pure-Rust static verification that both `app/cst-updater.exe` and `app/cst-server.exe` embed
/// the exact `\n`-delimited `CST_UPDATE_ROOT_V1` block for `(expected_manifest_url, expected_public_key_b64)`,
/// with zero conflicting `CST_UPDATE_ROOT_V1` blocks.
pub fn verify_static_binary_update_root(
    dist_dir: &Path,
    expected_manifest_url: &str,
    expected_public_key_b64: &str,
) -> Result<(), String> {
    let marker = format_update_root_marker(expected_manifest_url, expected_public_key_b64);
    for rel in [UPDATER_EXE_PATH, SERVER_EXE_PATH] {
        let bytes = fs::read(dist_dir.join(rel)).map_err(|e| format!("无法读取 {rel}：{e}"))?;
        verify_exact_single_root_marker(&bytes, &marker, rel, expected_manifest_url)?;
    }
    Ok(())
}

pub fn resolve_update_build_config(
    channel: Option<DistChannel>,
    require: bool,
) -> Result<Option<UpdateBuildConfig>, String> {
    let env_url = env::var(ENV_MANIFEST_URL).ok();
    let env_key = env::var(ENV_PUBLIC_KEY).ok();
    resolve_update_build_config_from_values(
        channel,
        require,
        env_url.as_deref(),
        env_key.as_deref(),
    )
}

pub fn resolve_update_build_config_from_values(
    channel: Option<DistChannel>,
    require: bool,
    env_url: Option<&str>,
    env_key: Option<&str>,
) -> Result<Option<UpdateBuildConfig>, String> {
    let u_opt = env_url.map(str::trim).filter(|s| !s.is_empty());
    let k_opt = env_key.map(str::trim).filter(|s| !s.is_empty());

    let (url, key_b64) = match channel {
        Some(DistChannel::Disabled) => {
            if require {
                return Err("--channel disabled 不能与 --require-update-config 同时使用。".into());
            }
            return Ok(None);
        }
        Some(DistChannel::Stable) | Some(DistChannel::Beta) => {
            let target_url = if channel == Some(DistChannel::Beta) {
                OFFICIAL_BETA_MANIFEST_URL.to_owned()
            } else {
                OFFICIAL_MANIFEST_URL.to_owned()
            };
            let target_key = match k_opt {
                Some(k) if is_disabled_update_sentinel(k) => {
                    return Err(format!(
                        "显式指定在线通道时环境变量 {ENV_PUBLIC_KEY} 不能设为禁用值。"
                    ));
                }
                Some(k) => k.to_owned(),
                None => OFFICIAL_PUBLIC_KEY_B64.to_owned(),
            };
            (target_url, target_key)
        }
        None => {
            if u_opt.is_some_and(is_disabled_update_sentinel)
                || k_opt.is_some_and(is_disabled_update_sentinel)
            {
                if require {
                    return Err(format!(
                        "启用 --require-update-config 时 {ENV_MANIFEST_URL} 与 {ENV_PUBLIC_KEY} 不能设为禁用值。"
                    ));
                }
                return Ok(None);
            }
            (
                u_opt.unwrap_or(OFFICIAL_MANIFEST_URL).to_owned(),
                k_opt.unwrap_or(OFFICIAL_PUBLIC_KEY_B64).to_owned(),
            )
        }
    };

    let parsed = validate_release_url(&url)?;
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(format!("{ENV_MANIFEST_URL} 不允许包含查询参数或片段。"));
    }
    let (_, _, fp) = parse_canonical_public_key(&key_b64)?;
    Ok(Some(UpdateBuildConfig {
        manifest_url: url,
        public_key_b64: key_b64,
        public_key_fingerprint: fp,
    }))
}

pub fn expected_channel_manifest_url(base_url: &str, channel: &str) -> Result<String, String> {
    let base = base_url.trim().trim_end_matches('/');
    let parsed_base = validate_release_url(base)?;
    if parsed_base.query().is_some() || parsed_base.fragment().is_some() {
        return Err("公开基址 URL 不允许包含查询参数或片段。".into());
    }
    match channel {
        "stable" => Ok(format!("{base}/latest")),
        "beta" => Ok(format!("{base}/beta")),
        other => Err(format!(
            "不支持的发布通道：{other}（仅允许 stable 或 beta）。"
        )),
    }
}

pub fn run_keygen(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let mut out_path: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => {
                let raw = args.next().ok_or("--out 需要私钥输出路径。")?;
                out_path = Some(PathBuf::from(raw));
            }
            other => return Err(format!("未知的 keygen 参数：{other}")),
        }
    }
    let out_path = out_path.ok_or("keygen 必须通过 --out <key-file> 指定私钥文件路径。")?;
    let (pub_b64, fp) = generate_keypair_file(&out_path)?;
    println!("[xtask] keygen OK");
    println!("publicKey={pub_b64}");
    println!("fingerprint={fp}");
    println!("privateKeyFile={}", out_path.display());
    Ok(())
}

pub fn generate_keypair_file(out_path: &Path) -> Result<(String, String), String> {
    if out_path.exists() {
        return Err(format!(
            "私钥目标文件已存在，拒绝覆盖：{}",
            out_path.display()
        ));
    }
    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|e| format!("无法创建私钥目录 {}：{e}", parent.display()))?;
    }

    let (seed, verifying_bytes) = {
        let mut chosen = None;
        for _ in 0..8 {
            let mut candidate = Zeroizing::new([0u8; 32]);
            rand::fill(candidate.as_mut());
            let signing = SigningKey::from_bytes(&candidate);
            let pub_bytes = *signing.verifying_key().as_bytes();
            if ReleaseKey::from_bytes(&pub_bytes).is_ok() {
                chosen = Some((candidate, pub_bytes));
                break;
            }
        }
        chosen.ok_or("无法生成合法的 Ed25519 密钥对。")?
    };

    let mut encoded_seed = Zeroizing::new(STANDARD.encode(seed.as_slice()));
    encoded_seed.push('\n');

    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts
        .open(out_path)
        .map_err(|e| format!("无法创建私钥文件 {}：{e}", out_path.display()))?;
    file.write_all(encoded_seed.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("写入私钥文件失败：{e}"))?;

    let pub_b64 = STANDARD.encode(verifying_bytes);
    let fp = public_key_fingerprint(&verifying_bytes);
    Ok((pub_b64, fp))
}

fn load_signing_key(path: &Path) -> Result<(SigningKey, ReleaseKey, String), String> {
    let raw_bytes = Zeroizing::new(
        fs::read(path).map_err(|e| format!("无法读取私钥文件 {}：{e}", path.display()))?,
    );
    let text = Zeroizing::new(
        std::str::from_utf8(&raw_bytes)
            .map_err(|_| "私钥文件必须为 UTF-8 编码。".to_owned())?
            .trim()
            .to_owned(),
    );
    let decoded = Zeroizing::new(
        STANDARD
            .decode(text.as_bytes())
            .map_err(|_| "私钥文件不是有效的标准 Base64 编码。".to_owned())?,
    );
    let seed: Zeroizing<[u8; 32]> = Zeroizing::new(
        decoded
            .as_slice()
            .try_into()
            .map_err(|_| "私钥必须为 32 字节 Ed25519 种子。".to_owned())?,
    );
    let signing = SigningKey::from_bytes(&seed);
    let pub_bytes = *signing.verifying_key().as_bytes();
    let release_key = ReleaseKey::from_bytes(&pub_bytes)?;
    let pub_b64 = STANDARD.encode(pub_bytes);
    Ok((signing, release_key, pub_b64))
}

struct TempWorkDir(PathBuf);

impl TempWorkDir {
    fn create(prefix: &str) -> Result<Self, String> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = env::temp_dir().join(format!(
            "{prefix}-{}-{stamp}-{}",
            std::process::id(),
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&dir)
            .map_err(|e| format!("无法创建隔离工作目录 {}：{e}", dir.display()))?;
        Ok(Self(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempWorkDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub struct SignReleaseArgs {
    pub dist_dir: Option<PathBuf>,
    pub zip_path: PathBuf,
    pub attestation_path: PathBuf,
    pub expected_attestation_sha256: String,
    pub expected_commit: String,
    pub version: Option<String>,
    pub min_upgradable: String,
    pub base_url: String,
    pub notes_file: Option<PathBuf>,
    pub notes_inline: Option<String>,
    pub out_path: PathBuf,
    pub private_key_file: PathBuf,
}

fn parse_sign_release_args(
    mut args: impl Iterator<Item = String>,
) -> Result<SignReleaseArgs, String> {
    let mut dist_dir: Option<PathBuf> = None;
    let mut zip_path: Option<PathBuf> = None;
    let mut attestation_path: Option<PathBuf> = None;
    let mut expected_attestation_sha256: Option<String> = None;
    let mut expected_commit: Option<String> = None;
    let mut version: Option<String> = None;
    let mut min_upgradable: Option<String> = None;
    let mut base_url = DEFAULT_PUBLIC_BASE_URL.to_owned();
    let mut notes_file: Option<PathBuf> = None;
    let mut notes_inline: Option<String> = None;
    let mut out_path: Option<PathBuf> = None;
    let mut private_key_file: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dist-dir" => {
                dist_dir = Some(PathBuf::from(
                    args.next().ok_or("--dist-dir 需要干净发行树目录。")?,
                ));
            }
            "--zip" => {
                zip_path = Some(PathBuf::from(
                    args.next().ok_or("--zip 需要发行 ZIP 路径。")?,
                ));
            }
            "--attestation" => {
                attestation_path = Some(PathBuf::from(
                    args.next()
                        .ok_or("--attestation 需要无私钥构建机产出的构建证明文件路径。")?,
                ));
            }
            "--expected-attestation-sha256" => {
                expected_attestation_sha256 = Some(args.next().ok_or(
                    "--expected-attestation-sha256 需要构建证明文件的 64 位 SHA-256 摘要。",
                )?);
            }
            "--expected-commit" => {
                expected_commit = Some(
                    args.next()
                        .ok_or("--expected-commit 需要获准发布的完整 40 位 Git 提交哈希。")?,
                );
            }
            "--version" => {
                version = Some(args.next().ok_or("--version 需要版本号。")?);
            }
            "--min-upgradable" => {
                min_upgradable = Some(
                    args.next()
                        .ok_or("--min-upgradable 需要最低可升级版本号。")?,
                );
            }
            "--base-url" => {
                base_url = args.next().ok_or("--base-url 需要公开基址 URL。")?;
            }
            "--notes-file" => {
                notes_file = Some(PathBuf::from(
                    args.next().ok_or("--notes-file 需要更新说明文件路径。")?,
                ));
            }
            "--notes" => {
                notes_inline = Some(args.next().ok_or("--notes 需要更新说明文本。")?);
            }
            "--out" => {
                out_path = Some(PathBuf::from(
                    args.next().ok_or("--out 需要签名清单输出路径。")?,
                ));
            }
            "--private-key-file" => {
                private_key_file = Some(PathBuf::from(
                    args.next().ok_or("--private-key-file 需要私钥文件路径。")?,
                ));
            }
            other => return Err(format!("未知的 sign-release 参数：{other}")),
        }
    }

    let zip_path = zip_path.ok_or("sign-release 必须指定 --zip <path>。")?;
    let attestation_path =
        attestation_path.ok_or("sign-release 必须通过 --attestation <path> 指定构建证明文件。")?;
    let expected_attestation_sha256 = expected_attestation_sha256
        .ok_or("sign-release 必须通过命令行参数 --expected-attestation-sha256 <64-hex> 指定带外核验的构建证明 SHA-256。")?
        .trim()
        .to_ascii_lowercase();
    if !is_lower_hex_len(&expected_attestation_sha256, 64) {
        return Err("--expected-attestation-sha256 必须为 64 位小写十六进制 SHA-256 摘要。".into());
    }
    let expected_commit = expected_commit
        .ok_or("sign-release 必须通过命令行参数 --expected-commit <40-hex> 指定获准发布的 Git 提交哈希。")?
        .trim()
        .to_ascii_lowercase();
    if !is_lower_hex_len(&expected_commit, 40) {
        return Err("--expected-commit 必须为完整 40 位小写十六进制 Git 提交哈希。".into());
    }
    let min_upgradable =
        min_upgradable.ok_or("sign-release 必须指定 --min-upgradable <vX.Y.Z>。")?;
    let out_path = out_path.ok_or("sign-release 必须指定 --out <manifest.signed.json>。")?;
    let private_key_file = resolve_private_key_path(private_key_file)?;
    if notes_file.is_some() && notes_inline.is_some() {
        return Err("--notes-file 与 --notes 不能同时指定。".into());
    }

    Ok(SignReleaseArgs {
        dist_dir,
        zip_path,
        attestation_path,
        expected_attestation_sha256,
        expected_commit,
        version,
        min_upgradable,
        base_url,
        notes_file,
        notes_inline,
        out_path,
        private_key_file,
    })
}

fn resolve_private_key_path(explicit: Option<PathBuf>) -> Result<PathBuf, String> {
    match explicit {
        Some(p) => Ok(p),
        None => env::var(ENV_PRIVATE_KEY_FILE)
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| {
                format!(
                    "必须通过 --private-key-file 或环境变量 {ENV_PRIVATE_KEY_FILE} 指定私钥文件。"
                )
            }),
    }
}

pub fn run_sign_release(raw_args: impl Iterator<Item = String>) -> Result<(), String> {
    let args = parse_sign_release_args(raw_args)?;
    let summary = sign_release(&args)?;
    println!(
        "[xtask] sign-release PASS: version={} size={} unpackedSize={} sha256={}",
        summary.version, summary.archive_size, summary.unpacked_size, summary.archive_sha256
    );
    println!("[xtask] archive.url={}", summary.archive_url);
    println!("[xtask] manifest={}", args.out_path.display());
    Ok(())
}

pub struct SignReleaseSummary {
    pub version: String,
    pub archive_url: String,
    pub archive_size: u64,
    pub unpacked_size: u64,
    pub archive_sha256: String,
}

/// Offline release signing: strictly verifies pure cryptographic digests against the trusted
/// build attestation produced by `xtask dist` on the keyless build host. Never executes any
/// binary from `--dist-dir` or `--zip` under the signing account.
pub fn sign_release(args: &SignReleaseArgs) -> Result<SignReleaseSummary, String> {
    let extracted_holder;
    let dist_dir: &Path = match &args.dist_dir {
        Some(dir) => dir.as_path(),
        None => {
            extracted_holder = TempWorkDir::create("cst-sign-extract")?;
            let kit_dir = extracted_holder.path().join("cst-pilot");
            fs::create_dir_all(&kit_dir)
                .map_err(|e| format!("无法创建临时解包目录 {}：{e}", kit_dir.display()))?;
            cst_updater::verify::extract_clean_dist_archive(
                &args.zip_path,
                &kit_dir,
                cst_updater::manifest::MAX_UNPACKED_BYTES,
            )?;
            extracted_holder.path()
        }
    };
    let resolved_dist_dir = if args.dist_dir.is_some() {
        dist_dir.to_path_buf()
    } else {
        dist_dir.join("cst-pilot")
    };

    let version = match &args.version {
        Some(v) => v.clone(),
        None => cst_updater::layout::read_version(&resolved_dist_dir)?,
    };
    if version.contains('+') {
        return Err("发布版本号不能包含 + 构建元数据，需与服务端路由规则一致。".into());
    }
    let parsed_ver = cst_updater::manifest::version(&version)?;
    let parsed_min = cst_updater::manifest::version(&args.min_upgradable)?;
    if parsed_min > parsed_ver {
        return Err("min-upgradable 版本不能高于目标版本。".into());
    }
    if same_existing_file(&args.out_path, &args.private_key_file)
        || same_existing_file(&args.out_path, &args.zip_path)
        || same_existing_file(&args.out_path, &args.attestation_path)
    {
        return Err("--out 输出路径不能指向私钥文件、发行 ZIP 或构建证明文件。".into());
    }

    let base_url = args.base_url.trim().trim_end_matches('/');
    let parsed_base = validate_release_url(base_url)?;
    if parsed_base.query().is_some() || parsed_base.fragment().is_some() {
        return Err("--base-url 不允许包含查询参数或片段。".into());
    }

    let expected_commit = args.expected_commit.trim().to_ascii_lowercase();
    if !is_lower_hex_len(&expected_commit, 40) {
        return Err("--expected-commit 必须为完整 40 位小写十六进制 Git 提交哈希。".into());
    }

    let attestation = read_and_verify_build_attestation(
        &args.attestation_path,
        &args.expected_attestation_sha256,
    )?;
    if attestation.schema_version != 1 || attestation.platform != "windows-x86_64" {
        return Err("构建证明 schemaVersion 或 platform 不受支持。".into());
    }
    if !attestation.update_enabled || !attestation.verified_key_probe {
        return Err(
            "构建证明未通过无私钥构建环境的双程序公钥自检（updateEnabled / verifiedKeyProbe 必须为 true），拒绝签名。"
                .into(),
        );
    }
    if attestation.version != version {
        return Err(format!(
            "构建证明版本（{}）与待签版本（{version}）不一致，拒绝签名。",
            attestation.version
        ));
    }
    if !is_lower_hex_len(&attestation.git_commit, 40) || attestation.git_commit != expected_commit {
        return Err(format!(
            "构建证明提交哈希（{}）与获准发布提交（{expected_commit}）不一致，拒绝签名。",
            attestation.git_commit
        ));
    }
    let parsed_att_url = validate_release_url(&attestation.manifest_url)?;
    if parsed_att_url.query().is_some()
        || parsed_att_url.fragment().is_some()
        || !attestation
            .manifest_url
            .starts_with(&format!("{base_url}/"))
    {
        return Err(format!(
            "构建证明清单地址（{}）与当前发布基址（{base_url}）不匹配，拒绝签名。",
            attestation.manifest_url
        ));
    }

    let (signing_key, release_key, _) = load_signing_key(&args.private_key_file)?;
    let signing_pub_fp = release_key.fingerprint();
    if attestation.update_public_key_fingerprint != signing_pub_fp {
        return Err(format!(
            "构建证明公钥指纹（{}）与当前签名私钥公钥指纹（{signing_pub_fp}）不一致，拒绝签名。",
            attestation.update_public_key_fingerprint
        ));
    }

    cst_updater::verify::verify_clean_dist_tree(&resolved_dist_dir, &version)?;

    let sums_bytes = fs::read(resolved_dist_dir.join(SHA256SUMS_PATH))
        .map_err(|e| format!("无法读取 {SHA256SUMS_PATH}：{e}"))?;
    let actual_sums_sha256 = format!("{:x}", Sha256::digest(&sums_bytes));
    if actual_sums_sha256 != attestation.sha256_sums_sha256 {
        return Err("待签发行树 app/SHA256SUMS 摘要与构建证明不一致，拒绝签名。".into());
    }
    let sha256_sums = String::from_utf8(sums_bytes)
        .map_err(|_| format!("{SHA256SUMS_PATH} 必须为 UTF-8 编码。"))?;

    let actual_updater_sha256 = format!(
        "{:x}",
        Sha256::digest(
            fs::read(resolved_dist_dir.join(UPDATER_EXE_PATH))
                .map_err(|e| format!("无法读取 {UPDATER_EXE_PATH}：{e}"))?
        )
    );
    if actual_updater_sha256 != attestation.updater_exe_sha256 {
        return Err(format!(
            "待签 {UPDATER_EXE_PATH} 摘要与构建证明不一致，拒绝签名。"
        ));
    }
    let actual_server_sha256 = format!(
        "{:x}",
        Sha256::digest(
            fs::read(resolved_dist_dir.join(SERVER_EXE_PATH))
                .map_err(|e| format!("无法读取 {SERVER_EXE_PATH}：{e}"))?
        )
    );
    if actual_server_sha256 != attestation.server_exe_sha256 {
        return Err(format!(
            "待签 {SERVER_EXE_PATH} 摘要与构建证明不一致，拒绝签名。"
        ));
    }

    let build_info = read_and_validate_build_info(&resolved_dist_dir, &version, &signing_pub_fp)?;
    if build_info.built_at != attestation.built_at {
        return Err("BUILD-INFO.json 中的 builtAt 与构建证明不一致，拒绝签名。".into());
    }

    let release_notes = read_bounded_release_notes(&args.notes_file, &args.notes_inline)?;
    let (zip_bytes, archive_size, archive_sha256, unpacked_size) =
        read_and_inspect_zip(&args.zip_path, &sha256_sums)?;
    let archive_filename = format!("cst-pilot-{version}-windows-x64.zip");
    if attestation.archive_filename != archive_filename
        || attestation.archive_size != archive_size
        || attestation.archive_sha256 != archive_sha256
        || attestation.unpacked_size != unpacked_size
    {
        return Err("发行 ZIP 的文件名、体积或 SHA-256 与构建证明不一致，拒绝签名。".into());
    }

    emit_signed_manifest(
        &signing_key,
        &release_key,
        &version,
        &build_info.built_at,
        &args.min_upgradable,
        base_url,
        release_notes,
        sha256_sums,
        &zip_bytes,
        archive_size,
        unpacked_size,
        archive_sha256,
        &args.out_path,
    )
}

struct ValidatedBuildInfo {
    built_at: String,
}

fn read_and_validate_build_info(
    dist_dir: &Path,
    expected_version: &str,
    expected_pub_fp: &str,
) -> Result<ValidatedBuildInfo, String> {
    let build_info_bytes = fs::read(dist_dir.join(BUILD_INFO_PATH))
        .map_err(|e| format!("无法读取 {BUILD_INFO_PATH}：{e}"))?;
    let build_info: serde_json::Value =
        serde_json::from_slice(&build_info_bytes).map_err(|_| "BUILD-INFO.json 格式无效。")?;
    if build_info
        .get("version")
        .and_then(serde_json::Value::as_str)
        != Some(expected_version)
    {
        return Err("BUILD-INFO.json 中的 version 与发行包 VERSION 不一致。".into());
    }
    if build_info
        .get("platform")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|p| p != "windows-x86_64")
    {
        return Err("BUILD-INFO.json 中的 platform 必须为 windows-x86_64。".into());
    }
    if build_info
        .get("updateEnabled")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return Err(
            "BUILD-INFO.json 缺少 updateEnabled=true，该发行包未启用在线更新，拒绝签名。".into(),
        );
    }
    let Some(built_fp) = build_info
        .get("updatePublicKeyFingerprint")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Err(
            "BUILD-INFO.json 缺少 updatePublicKeyFingerprint，请重新构建在线更新包后再签名。"
                .into(),
        );
    };
    if built_fp != expected_pub_fp {
        return Err(format!(
            "签名私钥对应公钥指纹（{expected_pub_fp}）与构建内嵌公钥指纹（{built_fp}）不一致，拒绝签名。"
        ));
    }
    let built_at = build_info
        .get("builtAt")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or("BUILD-INFO.json 缺少 builtAt 时间戳。")?;
    chrono::DateTime::parse_from_rfc3339(&built_at)
        .map_err(|_| "BUILD-INFO.json 中的 builtAt 不是有效的 RFC 3339 时间戳。")?;
    Ok(ValidatedBuildInfo { built_at })
}

fn read_bounded_release_notes(
    notes_file: &Option<PathBuf>,
    notes_inline: &Option<String>,
) -> Result<String, String> {
    let release_notes = if let Some(notes_path) = notes_file {
        fs::read_to_string(notes_path)
            .map_err(|e| format!("无法读取更新说明 {}：{e}", notes_path.display()))?
    } else {
        notes_inline.clone().unwrap_or_default()
    };
    if release_notes.len() > 16 * 1024 {
        return Err("更新说明超过 16 KiB 上限。".into());
    }
    Ok(release_notes)
}

fn read_and_inspect_zip(
    zip_path: &Path,
    expected_sums: &str,
) -> Result<(Vec<u8>, u64, String, u64), String> {
    let zip_meta = fs::metadata(zip_path)
        .map_err(|e| format!("无法读取发行 ZIP {}：{e}", zip_path.display()))?;
    if zip_meta.len() == 0 || zip_meta.len() > MAX_SERVER_ARCHIVE_BYTES {
        return Err(format!(
            "发行 ZIP 体积（{} 字节）超出服务端默认上限（{} 字节）。",
            zip_meta.len(),
            MAX_SERVER_ARCHIVE_BYTES
        ));
    }
    let zip_bytes =
        fs::read(zip_path).map_err(|e| format!("无法读取发行 ZIP {}：{e}", zip_path.display()))?;
    let archive_size = zip_bytes.len() as u64;
    let archive_sha256 = format!("{:x}", Sha256::digest(&zip_bytes));
    let unpacked_size = inspect_zip_unpacked_size(&zip_bytes, expected_sums)?;
    Ok((zip_bytes, archive_size, archive_sha256, unpacked_size))
}

#[allow(clippy::too_many_arguments)]
fn emit_signed_manifest(
    signing_key: &SigningKey,
    release_key: &ReleaseKey,
    version: &str,
    built_at: &str,
    min_upgradable: &str,
    base_url: &str,
    release_notes: String,
    sha256_sums: String,
    zip_bytes: &[u8],
    archive_size: u64,
    unpacked_size: u64,
    archive_sha256: String,
    out_path: &Path,
) -> Result<SignReleaseSummary, String> {
    let archive_filename = format!("cst-pilot-{version}-windows-x64.zip");
    let archive_url = format!("{base_url}/files/{version}/{archive_filename}");
    validate_release_url(&archive_url)?;

    let sha256_signature = sign_domain(signing_key, SUMS_DOMAIN, sha256_sums.as_bytes());
    let manifest = ReleaseManifest {
        schema_version: 1,
        platform: "windows-x86_64".to_owned(),
        version: version.to_owned(),
        built_at: built_at.to_owned(),
        min_upgradable_version: min_upgradable.to_owned(),
        release_notes,
        archive: Archive {
            url: archive_url.clone(),
            size: archive_size,
            unpacked_size,
            sha256: archive_sha256.clone(),
        },
        sha256_sums,
        sha256_signature,
    };
    let payload =
        serde_json::to_string(&manifest).map_err(|_| "ReleaseManifest 紧凑序列化失败。")?;
    let signature = sign_domain(signing_key, MANIFEST_DOMAIN, payload.as_bytes());
    let signed = SignedManifest { payload, signature };
    let mut envelope_bytes =
        serde_json::to_vec(&signed).map_err(|_| "SignedManifest 序列化失败。")?;
    envelope_bytes.push(b'\n');

    // Offline closed-loop verification using the exact `cst-updater` verifier.
    let verified = release_key.verify(&envelope_bytes)?;
    verify_staged_offline(&verified, zip_bytes)?;

    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|e| format!("无法创建清单输出目录 {}：{e}", parent.display()))?;
    }
    fs::write(out_path, &envelope_bytes)
        .map_err(|e| format!("无法写入签名清单 {}：{e}", out_path.display()))?;

    Ok(SignReleaseSummary {
        version: version.to_owned(),
        archive_url,
        archive_size,
        unpacked_size,
        archive_sha256,
    })
}

fn sign_domain(signing_key: &SigningKey, domain: &[u8], data: &[u8]) -> String {
    let mut buf = Vec::with_capacity(domain.len() + data.len());
    buf.extend_from_slice(domain);
    buf.extend_from_slice(data);
    STANDARD.encode(signing_key.sign(&buf).to_bytes())
}

fn same_existing_file(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

fn inspect_zip_unpacked_size(zip_bytes: &[u8], expected_sums: &str) -> Result<u64, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes))
        .map_err(|e| format!("发行 ZIP 格式无效：{e}"))?;
    let mut unpacked_size = 0u64;
    let mut saw_sums = false;
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| format!("读取发行 ZIP 条目失败：{e}"))?;
        if entry.is_dir() {
            continue;
        }
        unpacked_size = unpacked_size
            .checked_add(entry.size())
            .ok_or("发行 ZIP 解压总字节数溢出。")?;
        if entry.name() == format!("cst-pilot/{SHA256SUMS_PATH}") {
            if entry.size() > MAX_SHA256SUMS_BYTES {
                return Err(format!("ZIP 内 {SHA256SUMS_PATH} 超过 6 MiB 上限。"));
            }
            let mut inner = Vec::new();
            entry
                .take(MAX_SHA256SUMS_BYTES + 1)
                .read_to_end(&mut inner)
                .map_err(|e| format!("读取 ZIP 内 {SHA256SUMS_PATH} 失败：{e}"))?;
            if inner.len() as u64 > MAX_SHA256SUMS_BYTES || inner != expected_sums.as_bytes() {
                return Err(format!(
                    "发行 ZIP 内的 {SHA256SUMS_PATH} 与发行目录不一致。"
                ));
            }
            saw_sums = true;
        }
    }
    if !saw_sums {
        return Err(format!("发行 ZIP 缺少 cst-pilot/{SHA256SUMS_PATH}。"));
    }
    Ok(unpacked_size)
}

fn verify_staged_offline(
    verified: &cst_updater::manifest::VerifiedRelease,
    zip_bytes: &[u8],
) -> Result<(), String> {
    let temp = TempWorkDir::create("cst-sign-verify")?;
    let cancel = CancellationToken::new();
    #[cfg(windows)]
    {
        VerifiedPackage::stage_from_archive_bytes(temp.path(), verified.clone(), zip_bytes, &cancel)
            .map(drop)
    }
    #[cfg(not(windows))]
    {
        let archive_path = temp.path().join("package.zip");
        let program_dir = temp.path().join("program");
        fs::write(&archive_path, zip_bytes).map_err(|e| format!("无法写入离线验签压缩包：{e}"))?;
        fs::create_dir_all(&program_dir).map_err(|e| format!("无法创建离线验签解压目录：{e}"))?;
        cst_updater::verify::extract_archive(&archive_path, &program_dir, verified, &cancel)?;
        cst_updater::verify::verify_tree(&program_dir, verified, &cancel)
    }
}

pub struct PublishReleaseArgs {
    pub manifest_path: PathBuf,
    pub zip_path: PathBuf,
    pub channel: String,
    pub activate: bool,
    pub admin_url: String,
    pub public_base_url: String,
    pub public_manifest_url: Option<String>,
    pub public_archive_url: Option<String>,
    pub public_key_b64: String,
}

fn parse_publish_release_args(
    mut args: impl Iterator<Item = String>,
) -> Result<PublishReleaseArgs, String> {
    let mut manifest_path: Option<PathBuf> = None;
    let mut zip_path: Option<PathBuf> = None;
    let mut channel = "stable".to_owned();
    let mut activate = true;
    let mut admin_url = DEFAULT_ADMIN_URL.to_owned();
    let mut public_base_url = DEFAULT_PUBLIC_BASE_URL.to_owned();
    let mut public_manifest_url: Option<String> = None;
    let mut public_archive_url: Option<String> = None;
    let mut public_key_b64: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--manifest" => {
                manifest_path = Some(PathBuf::from(
                    args.next().ok_or("--manifest 需要签名清单文件路径。")?,
                ));
            }
            "--zip" => {
                zip_path = Some(PathBuf::from(
                    args.next().ok_or("--zip 需要发行 ZIP 路径。")?,
                ));
            }
            "--channel" => {
                channel = args.next().ok_or("--channel 需要 stable 或 beta。")?;
            }
            "--no-activate" => activate = false,
            "--admin-url" => {
                admin_url = args.next().ok_or("--admin-url 需要管理接口 URL。")?;
            }
            "--public-base-url" => {
                public_base_url = args.next().ok_or("--public-base-url 需要公开基址 URL。")?;
            }
            "--public-manifest-url" => {
                public_manifest_url = Some(
                    args.next()
                        .ok_or("--public-manifest-url 需要公开清单验证 URL。")?,
                );
            }
            "--public-archive-url" => {
                public_archive_url = Some(
                    args.next()
                        .ok_or("--public-archive-url 需要公开归档验证 URL。")?,
                );
            }
            "--public-key" => {
                public_key_b64 = Some(args.next().ok_or("--public-key 需要标准 Base64 公钥。")?);
            }
            "--token" | "--admin-token" => {
                return Err(format!(
                    "禁止通过命令行参数传递管理令牌，请使用环境变量 {ENV_ADMIN_TOKEN}。"
                ));
            }
            other => return Err(format!("未知的 publish-release 参数：{other}")),
        }
    }

    if !matches!(channel.as_str(), "stable" | "beta") {
        return Err(format!(
            "不支持的 --channel：{channel}（仅允许 stable 或 beta）。"
        ));
    }
    let manifest_path = manifest_path.ok_or("publish-release 必须指定 --manifest <path>。")?;
    let zip_path = zip_path.ok_or("publish-release 必须指定 --zip <path>。")?;
    let public_key_b64 = resolve_publish_public_key(public_key_b64)?;

    Ok(PublishReleaseArgs {
        manifest_path,
        zip_path,
        channel,
        activate,
        admin_url,
        public_base_url,
        public_manifest_url,
        public_archive_url,
        public_key_b64,
    })
}

fn resolve_publish_public_key(explicit: Option<String>) -> Result<String, String> {
    if let Some(v) = explicit {
        return Ok(v);
    }
    if let Some(env_val) = env::var(ENV_PUBLIC_KEY)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
    {
        if is_disabled_update_sentinel(&env_val) {
            return Err(format!(
                "环境变量 {ENV_PUBLIC_KEY} 当前设为禁用值，无法执行发布验签。"
            ));
        }
        return Ok(env_val);
    }
    Ok(OFFICIAL_PUBLIC_KEY_B64.to_owned())
}

fn read_admin_token_from_env() -> Result<Zeroizing<String>, String> {
    let token = Zeroizing::new(
        env::var(ENV_ADMIN_TOKEN)
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("缺少管理发布令牌环境变量 {ENV_ADMIN_TOKEN}。"))?,
    );
    if token.len() < 32 {
        return Err(format!("{ENV_ADMIN_TOKEN} 长度不能少于 32 字节。"));
    }
    Ok(token)
}

pub fn run_publish_release(raw_args: impl Iterator<Item = String>) -> Result<(), String> {
    let args = parse_publish_release_args(raw_args)?;
    let token = read_admin_token_from_env()?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("无法初始化发布运行时：{e}"))?;
    rt.block_on(publish_release(&args, &token))
}

pub async fn publish_release(
    args: &PublishReleaseArgs,
    admin_token: &Zeroizing<String>,
) -> Result<(), String> {
    let (release_key, _, _) = parse_canonical_public_key(&args.public_key_b64)?;
    let manifest_bytes = fs::read(&args.manifest_path)
        .map_err(|e| format!("无法读取签名清单 {}：{e}", args.manifest_path.display()))?;
    let verified = release_key.verify(&manifest_bytes)?;
    let archive_meta = fs::metadata(&args.zip_path)
        .map_err(|e| format!("无法读取发行 ZIP {}：{e}", args.zip_path.display()))?;
    if !archive_meta.is_file() {
        return Err("指定的 --zip 不是普通文件。".into());
    }
    if archive_meta.len() != verified.info().archive.size {
        return Err(format!(
            "本地 ZIP 体积（{}）与签名清单声明（{}）不一致。",
            archive_meta.len(),
            verified.info().archive.size
        ));
    }

    let mut upload_url = Url::parse(&args.admin_url)
        .map_err(|_| format!("无效的 --admin-url：{}", args.admin_url))?;
    if !matches!(upload_url.scheme(), "http" | "https") {
        return Err("--admin-url 仅允许 http 或 https。".into());
    }
    if upload_url.query().is_some() || upload_url.fragment().is_some() {
        return Err(
            "--admin-url 不能预先携带查询参数或片段，请通过 --channel 与 --no-activate 指定。"
                .into(),
        );
    }
    if upload_url.scheme() == "http" {
        let is_loopback = matches!(
            upload_url.host_str(),
            Some("127.0.0.1" | "::1" | "localhost")
        );
        if !is_loopback {
            return Err("非回环管理地址必须使用 https。".into());
        }
    }
    upload_url
        .query_pairs_mut()
        .append_pair("channel", &args.channel)
        .append_pair("activate", if args.activate { "true" } else { "false" });

    let boundary = format!("cstpilot-{}", Uuid::new_v4().simple());
    let archive_filename = format!("cst-pilot-{}-windows-x64.zip", verified.info().version);
    let mut prefix = Vec::with_capacity(manifest_bytes.len() + 512);
    prefix.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    prefix.extend_from_slice(
        b"Content-Disposition: form-data; name=\"manifest\"; filename=\"manifest.signed.json\"\r\n",
    );
    prefix.extend_from_slice(b"Content-Type: application/json\r\n\r\n");
    prefix.extend_from_slice(&manifest_bytes);
    prefix.extend_from_slice(format!("\r\n--{boundary}\r\n").as_bytes());
    prefix.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"archive\"; filename=\"{archive_filename}\"\r\n"
        )
        .as_bytes(),
    );
    prefix.extend_from_slice(b"Content-Type: application/zip\r\n\r\n");
    let suffix = Bytes::from(format!("\r\n--{boundary}--\r\n"));
    let total_len = prefix.len() as u64 + archive_meta.len() + suffix.len() as u64;

    let zip_file = tokio::fs::File::open(&args.zip_path)
        .await
        .map_err(|e| format!("无法打开归档文件流：{e}"))?;

    enum StreamStage {
        Prefix(Bytes, tokio::fs::File),
        Archive(tokio::fs::File),
        Done,
    }
    let body_stream = futures_util::stream::try_unfold(
        StreamStage::Prefix(Bytes::from(prefix), zip_file),
        move |stage| {
            let suffix = suffix.clone();
            async move {
                match stage {
                    StreamStage::Prefix(bytes, file) => {
                        Ok::<_, std::io::Error>(Some((bytes, StreamStage::Archive(file))))
                    }
                    StreamStage::Archive(mut file) => {
                        let mut buf = vec![0u8; 64 * 1024];
                        let n = file.read(&mut buf).await?;
                        if n == 0 {
                            Ok(Some((suffix, StreamStage::Done)))
                        } else {
                            buf.truncate(n);
                            Ok(Some((Bytes::from(buf), StreamStage::Archive(file))))
                        }
                    }
                    StreamStage::Done => Ok(None),
                }
            }
        },
    );

    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| format!("无法初始化上传 HTTP 客户端：{e}"))?;

    println!(
        "[xtask] Uploading {} ({} bytes) to channel={} (activate={})...",
        verified.info().version,
        archive_meta.len(),
        args.channel,
        args.activate
    );
    let mut auth_val =
        reqwest::header::HeaderValue::from_str(&format!("Bearer {}", admin_token.as_str()))
            .map_err(|_| "管理令牌包含非法 HTTP 头字符。".to_owned())?;
    auth_val.set_sensitive(true);
    let resp = client
        .post(upload_url)
        .header(reqwest::header::AUTHORIZATION, auth_val)
        .header(
            reqwest::header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header(reqwest::header::CONTENT_LENGTH, total_len)
        .body(reqwest::Body::wrap_stream(body_stream))
        .send()
        .await
        .map_err(|e| format!("上传发布包失败：{e}"))?;

    let status = resp.status();
    let resp_bytes = read_bounded_response(resp, MAX_ADMIN_RESPONSE_BYTES)
        .await
        .map_err(|e| format!("读取发布响应失败：{e}"))?;
    if status != reqwest::StatusCode::CREATED {
        let code = serde_json::from_slice::<serde_json::Value>(&resp_bytes)
            .ok()
            .and_then(|v| v.get("error")?.get("code")?.as_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".to_owned());
        return Err(format!(
            "服务端拒绝发布（HTTP {}, error.code={code}）。",
            status.as_u16()
        ));
    }
    let ack: serde_json::Value =
        serde_json::from_slice(&resp_bytes).map_err(|_| "发布响应不是合法 JSON。")?;
    let ack_channel = ack.get("channel").and_then(serde_json::Value::as_str);
    let ack_activated = ack.get("activated").and_then(serde_json::Value::as_bool);
    let ack_version = ack
        .get("record")
        .and_then(|r| r.get("version"))
        .and_then(serde_json::Value::as_str);
    if ack_channel != Some(args.channel.as_str())
        || ack_activated != Some(args.activate)
        || ack_version != Some(verified.info().version.as_str())
    {
        return Err(format!(
            "服务端返回发布结果与预期不符（channel={ack_channel:?}, activated={ack_activated:?}, version={ack_version:?}）。"
        ));
    }
    println!(
        "[xtask] Upload accepted (201 Created): version={} channel={} activated={}",
        verified.info().version,
        args.channel,
        args.activate
    );

    if args.activate {
        let public_base = args.public_base_url.trim().trim_end_matches('/');
        let manifest_verify_url = args.public_manifest_url.clone().unwrap_or_else(|| {
            if args.channel == "beta" {
                format!("{public_base}/beta")
            } else {
                format!("{public_base}/latest")
            }
        });
        let archive_verify_url = args
            .public_archive_url
            .clone()
            .unwrap_or_else(|| verified.info().archive.url.clone());

        let verify_client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| format!("无法初始化验收 HTTP 客户端：{e}"))?;

        let manifest_resp = verify_client
            .get(&manifest_verify_url)
            .send()
            .await
            .map_err(|e| format!("发布后拉取公开清单失败（{manifest_verify_url}）：{e}"))?;
        if manifest_resp.status() != reqwest::StatusCode::OK {
            return Err(format!(
                "发布后公开清单状态异常（{manifest_verify_url}）：HTTP {}",
                manifest_resp.status().as_u16()
            ));
        }
        let remote_manifest = read_bounded_response(manifest_resp, MAX_PUBLIC_MANIFEST_BYTES)
            .await
            .map_err(|e| format!("读取公开清单正文失败：{e}"))?;
        let remote_verified = release_key
            .verify(&remote_manifest)
            .map_err(|e| format!("发布后公开清单验签失败：{e}"))?;
        if remote_verified.info().version != verified.info().version
            || remote_manifest.as_slice() != manifest_bytes.as_slice()
        {
            return Err("发布后公开清单内容或版本与本地签名清单不一致。".into());
        }

        let head_resp = verify_client
            .head(&archive_verify_url)
            .send()
            .await
            .map_err(|e| format!("发布后核验归档 HEAD 失败（{archive_verify_url}）：{e}"))?;
        if head_resp.status() != reqwest::StatusCode::OK {
            return Err(format!(
                "发布后归档 HEAD 状态异常（{archive_verify_url}）：HTTP {}",
                head_resp.status().as_u16()
            ));
        }
        let head_len = head_resp
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());
        if head_len != Some(verified.info().archive.size) {
            return Err(format!(
                "发布后归档 Content-Length（{head_len:?}）与签名清单（{}）不一致。",
                verified.info().archive.size
            ));
        }
        println!(
            "[xtask] Post-publish verification PASS: manifest={} (verified), archive HEAD={} ({} bytes)",
            manifest_verify_url,
            archive_verify_url,
            verified.info().archive.size
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// One-shot `xtask release` pipeline (validate -> sign -> SSH tunnel -> upload -> verify)
// ---------------------------------------------------------------------------

pub struct ReleaseCommandArgs {
    pub package_path: PathBuf,
    pub min_upgradable: String,
    pub channel: String,
    pub version: Option<String>,
    pub attestation_path: Option<PathBuf>,
    pub expected_attestation_sha256: Option<String>,
    pub expected_commit: Option<String>,
    pub unattested_package: bool,
    pub notes_file: Option<PathBuf>,
    pub notes_inline: Option<String>,
    pub private_key_file: PathBuf,
    pub out_manifest: Option<PathBuf>,
    pub ssh_host: Option<String>,
    pub ssh_remote_port: u16,
    pub admin_url: Option<String>,
    pub public_base_url: String,
    pub public_manifest_url: Option<String>,
    pub public_archive_url: Option<String>,
    pub activate: bool,
    pub dry_run: bool,
}

pub fn validate_ssh_host(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return Err("--ssh-host 不能为空且长度不能超过 128 字符。".into());
    }
    if trimmed.starts_with('-') {
        return Err("--ssh-host 不能以 '-' 开头。".into());
    }
    if !trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'@'))
    {
        return Err("--ssh-host 仅允许包含 ASCII 字母、数字及 . - _ @ 字符。".into());
    }
    Ok(trimmed.to_owned())
}

fn resolve_trusted_ssh_binary() -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        let sys = cst_os::system_directory()?;
        let ssh = sys.join("OpenSSH").join("ssh.exe");
        let meta = fs::symlink_metadata(&ssh)
            .map_err(|e| format!("未找到系统内置 OpenSSH 客户端 {}：{e}", ssh.display()))?;
        if cst_os::is_reparse_or_symlink(&meta) || !meta.is_file() {
            return Err("系统 OpenSSH 客户端路径异常（存在重解析点或非普通文件）。".into());
        }
        Ok(ssh)
    }
    #[cfg(not(windows))]
    {
        for candidate in ["/usr/bin/ssh", "/bin/ssh"] {
            let p = Path::new(candidate);
            if p.is_file() {
                return Ok(p.to_path_buf());
            }
        }
        Err("未找到系统 /usr/bin/ssh 客户端。".into())
    }
}

struct ScopedSshTunnel {
    cancel: CancellationToken,
    worker: Option<tokio::task::JoinHandle<()>>,
    local_port: u16,
}

impl ScopedSshTunnel {
    /// Binds an in-process `127.0.0.1:0` listener held continuously by `xtask` (eliminating any
    /// local port-stealing TOCTOU window that could intercept `Authorization: Bearer`) and pipes
    /// accepted loopback streams directly over `ssh -W 127.0.0.1:<remote_port> -- <host>` stdio.
    async fn open(ssh_host: &str, remote_port: u16) -> Result<Self, String> {
        let host = validate_ssh_host(ssh_host)?;
        if remote_port == 0 {
            return Err("--ssh-remote-port 不能为 0。".into());
        }
        let ssh_bin = resolve_trusted_ssh_binary()?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| format!("无法绑定本机回环隧道监听端口：{e}"))?;
        let local_port = listener
            .local_addr()
            .map_err(|e| format!("无法读取本机回环隧道端口：{e}"))?
            .port();
        let target_spec = format!("127.0.0.1:{remote_port}");
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();

        let worker = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    biased;
                    _ = worker_cancel.cancelled() => break,
                    res = listener.accept() => res,
                };
                let Ok((tcp_stream, _)) = accepted else {
                    break;
                };
                let ssh_bin = ssh_bin.clone();
                let host = host.clone();
                let target_spec = target_spec.clone();
                let conn_cancel = worker_cancel.clone();
                tokio::spawn(async move {
                    bridge_tcp_over_ssh_stdio(
                        tcp_stream,
                        &ssh_bin,
                        &host,
                        &target_spec,
                        conn_cancel,
                    )
                    .await;
                });
            }
        });

        Ok(Self {
            cancel,
            worker: Some(worker),
            local_port,
        })
    }

    fn admin_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1/admin/releases", self.local_port)
    }

    async fn close(mut self) {
        self.cancel.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.await;
        }
    }
}

impl Drop for ScopedSshTunnel {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(worker) = &self.worker {
            worker.abort();
        }
    }
}

async fn bridge_tcp_over_ssh_stdio(
    tcp_stream: tokio::net::TcpStream,
    ssh_bin: &Path,
    host: &str,
    target_spec: &str,
    cancel: CancellationToken,
) {
    let mut cmd = tokio::process::Command::new(ssh_bin);
    cmd.env_remove(ENV_PRIVATE_KEY_FILE)
        .env_remove(ENV_ADMIN_TOKEN)
        .kill_on_drop(true)
        .args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "ConnectTimeout=15",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
            "-W",
            target_spec,
            "--",
            host,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let mut stream = tcp_stream;
            write_tunnel_502(&mut stream, &format!("启动系统 SSH 进程失败：{e}")).await;
            return;
        }
    };

    let Some(mut child_stdin) = child.stdin.take() else {
        return;
    };
    let Some(mut child_stdout) = child.stdout.take() else {
        return;
    };
    let child_stderr = child.stderr.take();
    let (mut tcp_read, mut tcp_write) = tcp_stream.into_split();

    let to_remote = async {
        let _ = tokio::io::copy(&mut tcp_read, &mut child_stdin).await;
        let _ = child_stdin.shutdown().await;
    };

    let mut received_from_remote = 0u64;
    let from_remote = async {
        let mut buf = [0u8; 32 * 1024];
        while let Ok(n) = child_stdout.read(&mut buf).await {
            if n == 0 {
                break;
            }
            received_from_remote = received_from_remote.saturating_add(n as u64);
            if tcp_write.write_all(&buf[..n]).await.is_err() {
                break;
            }
        }
    };

    tokio::select! {
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            return;
        }
        _ = async { tokio::join!(to_remote, from_remote) } => {}
    }

    if received_from_remote == 0 {
        let mut err_buf = Vec::new();
        if let Some(stderr) = child_stderr {
            let _ = stderr.take(4096).read_to_end(&mut err_buf).await;
        }
        let status = child.wait().await.ok();
        let detail = String::from_utf8_lossy(&err_buf).trim().to_owned();
        let msg = match (status, detail.is_empty()) {
            (Some(s), false) => format!("SSH 隧道转发失败（{s}）：{detail}"),
            (Some(s), true) => format!("SSH 隧道转发失败（{s}）。"),
            (None, false) => format!("SSH 隧道转发失败：{detail}"),
            (None, true) => "SSH 隧道未返回响应。".into(),
        };
        let body = serde_json::json!({
            "error": {
                "code": "ssh_tunnel_failed",
                "message": msg,
            }
        })
        .to_string();
        let hdr = format!(
            "HTTP/1.1 502 Bad Gateway\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = tcp_write.write_all(hdr.as_bytes()).await;
        let _ = tcp_write.write_all(body.as_bytes()).await;
    } else {
        let _ = child.wait().await;
    }
    let _ = tcp_write.shutdown().await;
}

async fn write_tunnel_502(stream: &mut tokio::net::TcpStream, msg: &str) {
    let body = serde_json::json!({
        "error": {
            "code": "ssh_tunnel_failed",
            "message": msg,
        }
    })
    .to_string();
    let hdr = format!(
        "HTTP/1.1 502 Bad Gateway\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(hdr.as_bytes()).await;
    let _ = stream.write_all(body.as_bytes()).await;
    let _ = stream.shutdown().await;
}

pub fn parse_release_command_args(
    mut args: impl Iterator<Item = String>,
) -> Result<ReleaseCommandArgs, String> {
    let mut package_path: Option<PathBuf> = None;
    let mut min_upgradable: Option<String> = None;
    let mut channel = "stable".to_owned();
    let mut version: Option<String> = None;
    let mut attestation_path: Option<PathBuf> = None;
    let mut expected_attestation_sha256: Option<String> = None;
    let mut expected_commit: Option<String> = None;
    let mut unattested_package = false;
    let mut notes_file: Option<PathBuf> = None;
    let mut notes_inline: Option<String> = None;
    let mut private_key_file: Option<PathBuf> = None;
    let mut out_manifest: Option<PathBuf> = None;
    let mut ssh_host: Option<String> = None;
    let mut ssh_remote_port: Option<u16> = None;
    let mut admin_url: Option<String> = None;
    let mut public_base_url = DEFAULT_PUBLIC_BASE_URL.to_owned();
    let mut public_manifest_url: Option<String> = None;
    let mut public_archive_url: Option<String> = None;
    let mut activate = true;
    let mut dry_run = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--package" => {
                package_path = Some(PathBuf::from(
                    args.next().ok_or("--package 需要发行包 ZIP 或目录路径。")?,
                ));
            }
            "--min-upgradable" => {
                min_upgradable = Some(
                    args.next()
                        .ok_or("--min-upgradable 需要最低可升级版本号。")?,
                );
            }
            "--channel" => {
                channel = args.next().ok_or("--channel 需要 stable 或 beta。")?;
            }
            "--version" => {
                version = Some(args.next().ok_or("--version 需要版本号。")?);
            }
            "--attestation" => {
                attestation_path = Some(PathBuf::from(
                    args.next().ok_or("--attestation 需要构建证明文件路径。")?,
                ));
            }
            "--expected-attestation-sha256" => {
                expected_attestation_sha256 = Some(
                    args.next()
                        .ok_or("--expected-attestation-sha256 需要 64 位 SHA-256 摘要。")?,
                );
            }
            "--expected-commit" => {
                expected_commit = Some(
                    args.next()
                        .ok_or("--expected-commit 需要完整 40 位 Git 提交哈希。")?,
                );
            }
            "--unattested-package" => unattested_package = true,
            "--notes-file" => {
                notes_file = Some(PathBuf::from(
                    args.next().ok_or("--notes-file 需要更新说明文件路径。")?,
                ));
            }
            "--notes" => {
                notes_inline = Some(args.next().ok_or("--notes 需要更新说明文本。")?);
            }
            "--private-key-file" => {
                private_key_file = Some(PathBuf::from(
                    args.next().ok_or("--private-key-file 需要私钥文件路径。")?,
                ));
            }
            "--out-manifest" => {
                out_manifest = Some(PathBuf::from(
                    args.next()
                        .ok_or("--out-manifest 需要签名清单输出文件路径。")?,
                ));
            }
            "--ssh-host" => {
                let raw = args.next().ok_or("--ssh-host 需要 SSH 目标别名或主机。")?;
                ssh_host = Some(validate_ssh_host(&raw)?);
            }
            "--ssh-remote-port" => {
                let raw = args.next().ok_or("--ssh-remote-port 需要远端端口号。")?;
                let p = raw
                    .parse::<u16>()
                    .map_err(|_| "--ssh-remote-port 端口号格式无效。")?;
                if p == 0 {
                    return Err("--ssh-remote-port 不能为 0。".into());
                }
                ssh_remote_port = Some(p);
            }
            "--admin-url" => {
                admin_url = Some(args.next().ok_or("--admin-url 需要管理接口 URL。")?);
            }
            "--public-base-url" => {
                public_base_url = args.next().ok_or("--public-base-url 需要公开基址 URL。")?;
            }
            "--public-manifest-url" => {
                public_manifest_url = Some(
                    args.next()
                        .ok_or("--public-manifest-url 需要公开清单验证 URL。")?,
                );
            }
            "--public-archive-url" => {
                public_archive_url = Some(
                    args.next()
                        .ok_or("--public-archive-url 需要公开归档验证 URL。")?,
                );
            }
            "--no-activate" => activate = false,
            "--dry-run" => dry_run = true,
            "--token" | "--admin-token" => {
                return Err(format!(
                    "禁止通过命令行参数传递管理令牌，请使用环境变量 {ENV_ADMIN_TOKEN}。"
                ));
            }
            other => return Err(format!("未知的 release 参数：{other}")),
        }
    }

    if !matches!(channel.as_str(), "stable" | "beta") {
        return Err(format!(
            "不支持的 --channel：{channel}（仅允许 stable 或 beta）。"
        ));
    }
    let package_path =
        package_path.ok_or("release 必须通过 --package <zip-or-dir> 指定待发行的包或目录。")?;
    let min_upgradable = min_upgradable.ok_or("release 必须指定 --min-upgradable <vX.Y.Z>。")?;
    if notes_file.is_some() && notes_inline.is_some() {
        return Err("--notes-file 与 --notes 不能同时指定。".into());
    }
    if ssh_host.is_some() && admin_url.is_some() {
        return Err("--ssh-host 与 --admin-url 不能同时指定。".into());
    }
    if ssh_remote_port.is_some() && ssh_host.is_none() {
        return Err("--ssh-remote-port 需配合 --ssh-host 使用。".into());
    }
    if unattested_package
        && (attestation_path.is_some()
            || expected_attestation_sha256.is_some()
            || expected_commit.is_some())
    {
        return Err(
            "--unattested-package 不能与 --attestation / --expected-attestation-sha256 / --expected-commit 同时指定。"
                .into(),
        );
    }
    if !unattested_package && (expected_attestation_sha256.is_none() || expected_commit.is_none()) {
        return Err(
            "默认构建证明模式必须同时指定 --expected-attestation-sha256 <64-hex> 与 --expected-commit <40-hex>（若发布外部独立包，请显式传入 --unattested-package）。"
                .into(),
        );
    }
    let private_key_file = resolve_private_key_path(private_key_file)?;

    Ok(ReleaseCommandArgs {
        package_path,
        min_upgradable,
        channel,
        version,
        attestation_path,
        expected_attestation_sha256,
        expected_commit,
        unattested_package,
        notes_file,
        notes_inline,
        private_key_file,
        out_manifest,
        ssh_host,
        ssh_remote_port: ssh_remote_port.unwrap_or(DEFAULT_SSH_REMOTE_PORT),
        admin_url,
        public_base_url,
        public_manifest_url,
        public_archive_url,
        activate,
        dry_run,
    })
}

pub fn run_release(raw_args: impl Iterator<Item = String>) -> Result<(), String> {
    let args = parse_release_command_args(raw_args)?;
    let admin_token = if args.dry_run {
        None
    } else {
        Some(read_admin_token_from_env()?)
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("无法初始化发布运行时：{e}"))?;
    rt.block_on(execute_release(&args, admin_token.as_ref()))
}

struct PreparedPackageInput {
    kit_dir: PathBuf,
    zip_path: PathBuf,
    version: String,
    discovered_attestation: Option<PathBuf>,
}

fn prepare_package_input(
    package_path: &Path,
    explicit_version: Option<&str>,
    work_dir: &Path,
) -> Result<PreparedPackageInput, String> {
    let meta = fs::symlink_metadata(package_path)
        .map_err(|e| format!("无法读取 --package 路径 {}：{e}", package_path.display()))?;
    if cst_os::is_reparse_or_symlink(&meta) {
        return Err(format!(
            "--package 路径禁止使用符号链接或重解析点：{}",
            package_path.display()
        ));
    }

    if meta.is_file() {
        let extracted_kit = work_dir.join("extracted-kit");
        fs::create_dir_all(&extracted_kit)
            .map_err(|e| format!("无法创建解包目录 {}：{e}", extracted_kit.display()))?;
        cst_updater::verify::extract_clean_dist_archive(
            package_path,
            &extracted_kit,
            cst_updater::manifest::MAX_UNPACKED_BYTES,
        )?;
        let disk_version = cst_updater::layout::read_version(&extracted_kit)?;
        if let Some(exp) = explicit_version
            && exp != disk_version
        {
            return Err(format!(
                "--version（{exp}）与包内 VERSION（{disk_version}）不一致。"
            ));
        }
        let canonical_name = format!("cst-pilot-{disk_version}-windows-x64.zip");
        let zip_path = if package_path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n == canonical_name)
        {
            package_path.to_path_buf()
        } else {
            let normalized = work_dir.join(&canonical_name);
            fs::copy(package_path, &normalized)
                .map_err(|e| format!("无法准备规范命名归档 {}：{e}", normalized.display()))?;
            normalized
        };
        let sibling_att = package_path.parent().and_then(|parent| {
            let candidate = parent.join(format!(
                "cst-pilot-{disk_version}-windows-x64.attestation.json"
            ));
            candidate.is_file().then_some(candidate)
        });
        return Ok(PreparedPackageInput {
            kit_dir: extracted_kit,
            zip_path,
            version: disk_version,
            discovered_attestation: sibling_att,
        });
    }

    if meta.is_dir() {
        // Subcase A: `--package` is an unpacked `cst-pilot` release tree directory (`VERSION` + `cst-pilot.exe`).
        if package_path.join("VERSION").is_file() && package_path.join("cst-pilot.exe").is_file() {
            let disk_version = cst_updater::layout::read_version(package_path)?;
            if let Some(exp) = explicit_version
                && exp != disk_version
            {
                return Err(format!(
                    "--version（{exp}）与目录内 VERSION（{disk_version}）不一致。"
                ));
            }
            cst_updater::verify::verify_clean_dist_tree(package_path, &disk_version)?;
            let sums = cst_updater::verify::scan_clean_dist_tree(package_path, &disk_version)?;
            let canonical_name = format!("cst-pilot-{disk_version}-windows-x64.zip");
            let sibling_zip = package_path.parent().and_then(|p| {
                let z = p.join(&canonical_name);
                z.is_file().then_some(z)
            });
            let zip_path = match sibling_zip {
                Some(z) => z,
                None => {
                    let generated = work_dir.join(&canonical_name);
                    super::write_release_zip(package_path, &generated, &sums)?;
                    generated
                }
            };
            let sibling_att = package_path.parent().and_then(|parent| {
                let candidate = parent.join(format!(
                    "cst-pilot-{disk_version}-windows-x64.attestation.json"
                ));
                candidate.is_file().then_some(candidate)
            });
            return Ok(PreparedPackageInput {
                kit_dir: package_path.to_path_buf(),
                zip_path,
                version: disk_version,
                discovered_attestation: sibling_att,
            });
        }

        // Subcase B: `--package` is a `dist` output parent directory containing `cst-pilot-<ver>-windows-x64.zip`.
        let located_zip = locate_zip_in_dist_parent(package_path, explicit_version)?;
        return prepare_package_input(&located_zip, explicit_version, work_dir);
    }

    Err(format!(
        "--package 路径不是普通文件或目录：{}",
        package_path.display()
    ))
}

fn locate_zip_in_dist_parent(
    parent_dir: &Path,
    explicit_version: Option<&str>,
) -> Result<PathBuf, String> {
    if let Some(ver) = explicit_version {
        let exact = parent_dir.join(format!("cst-pilot-{ver}-windows-x64.zip"));
        if exact.is_file() {
            return Ok(exact);
        }
        return Err(format!(
            "目录 {} 下未找到指定版本的发行归档 {}。",
            parent_dir.display(),
            exact.display()
        ));
    }
    let mut candidates = Vec::new();
    let entries = fs::read_dir(parent_dir)
        .map_err(|e| format!("无法读取目录 {}：{e}", parent_dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.starts_with("cst-pilot-")
            && name.ends_with("-windows-x64.zip")
            && path.is_file()
        {
            candidates.push(path);
        }
    }
    match candidates.len() {
        1 => Ok(candidates.remove(0)),
        0 => Err(format!(
            "目录 {} 下未找到 cst-pilot-*-windows-x64.zip 或已解压的发行目录。",
            parent_dir.display()
        )),
        _ => Err(format!(
            "目录 {} 下存在多个发行 ZIP，请通过 --version 或直接指定具体 ZIP 文件路径。",
            parent_dir.display()
        )),
    }
}

pub async fn execute_release(
    args: &ReleaseCommandArgs,
    admin_token: Option<&Zeroizing<String>>,
) -> Result<(), String> {
    let work_dir = TempWorkDir::create("cst-release-run")?;
    let prepared =
        prepare_package_input(&args.package_path, args.version.as_deref(), work_dir.path())?;
    let base_url = args.public_base_url.trim().trim_end_matches('/');
    let expected_channel_url = expected_channel_manifest_url(base_url, &args.channel)?;

    let out_manifest = args
        .out_manifest
        .clone()
        .unwrap_or_else(|| work_dir.path().join("manifest.signed.json"));
    if same_existing_file(&out_manifest, &args.package_path)
        || same_existing_file(&out_manifest, &prepared.zip_path)
        || same_existing_file(&out_manifest, &args.private_key_file)
    {
        return Err("--out-manifest 输出路径不能覆盖输入发行包或私钥文件。".into());
    }

    let (_, _, signing_pub_b64) = load_signing_key(&args.private_key_file)?;

    let summary = if args.unattested_package {
        sign_unattested_package(
            &prepared.kit_dir,
            &prepared.zip_path,
            &prepared.version,
            &args.min_upgradable,
            base_url,
            &expected_channel_url,
            &args.notes_file,
            &args.notes_inline,
            &out_manifest,
            &args.private_key_file,
        )?
    } else {
        let attestation_path = args
            .attestation_path
            .clone()
            .or(prepared.discovered_attestation)
            .ok_or_else(|| {
                format!(
                    "未找到构建证明文件 cst-pilot-{}-windows-x64.attestation.json（请传入 --attestation，或对独立外部包显式传入 --unattested-package）。",
                    prepared.version
                )
            })?;
        if same_existing_file(&out_manifest, &attestation_path) {
            return Err("--out-manifest 输出路径不能覆盖构建证明文件。".into());
        }
        let exp_att_sha = args
            .expected_attestation_sha256
            .clone()
            .ok_or("缺少 --expected-attestation-sha256。")?;
        let exp_commit = args
            .expected_commit
            .clone()
            .ok_or("缺少 --expected-commit。")?;
        let att = read_and_verify_build_attestation(&attestation_path, &exp_att_sha)?;
        if att.manifest_url != expected_channel_url {
            return Err(format!(
                "构建证明通道地址（{}）与目标发布通道 {}（期望 {expected_channel_url}）不一致，拒绝跨通道误发。",
                att.manifest_url, args.channel
            ));
        }
        sign_release(&SignReleaseArgs {
            dist_dir: Some(prepared.kit_dir),
            zip_path: prepared.zip_path.clone(),
            attestation_path,
            expected_attestation_sha256: exp_att_sha,
            expected_commit: exp_commit,
            version: Some(prepared.version),
            min_upgradable: args.min_upgradable.clone(),
            base_url: base_url.to_owned(),
            notes_file: args.notes_file.clone(),
            notes_inline: args.notes_inline.clone(),
            out_path: out_manifest.clone(),
            private_key_file: args.private_key_file.clone(),
        })?
    };

    println!(
        "[xtask] Package verified & signed: version={} channel={} size={} unpackedSize={} sha256={}",
        summary.version,
        args.channel,
        summary.archive_size,
        summary.unpacked_size,
        summary.archive_sha256
    );

    if args.dry_run {
        println!(
            "[xtask] release --dry-run PASS: archive.url={} manifest={}",
            summary.archive_url,
            out_manifest.display()
        );
        return Ok(());
    }

    let token =
        admin_token.ok_or_else(|| format!("缺少管理发布令牌环境变量 {ENV_ADMIN_TOKEN}。"))?;

    if let Some(ref ssh_host) = args.ssh_host {
        println!(
            "[xtask] Opening scoped SSH loopback tunnel to {ssh_host} (remote 127.0.0.1:{})...",
            args.ssh_remote_port
        );
        let tunnel = ScopedSshTunnel::open(ssh_host, args.ssh_remote_port).await?;
        let publish_args = PublishReleaseArgs {
            manifest_path: out_manifest,
            zip_path: prepared.zip_path,
            channel: args.channel.clone(),
            activate: args.activate,
            admin_url: tunnel.admin_url(),
            public_base_url: base_url.to_owned(),
            public_manifest_url: args.public_manifest_url.clone(),
            public_archive_url: args.public_archive_url.clone(),
            public_key_b64: signing_pub_b64,
        };
        let res = publish_release(&publish_args, token).await;
        tunnel.close().await;
        res
    } else {
        let publish_args = PublishReleaseArgs {
            manifest_path: out_manifest,
            zip_path: prepared.zip_path,
            channel: args.channel.clone(),
            activate: args.activate,
            admin_url: args
                .admin_url
                .clone()
                .unwrap_or_else(|| DEFAULT_ADMIN_URL.to_owned()),
            public_base_url: base_url.to_owned(),
            public_manifest_url: args.public_manifest_url.clone(),
            public_archive_url: args.public_archive_url.clone(),
            public_key_b64: signing_pub_b64,
        };
        publish_release(&publish_args, token).await
    }
}

#[allow(clippy::too_many_arguments)]
fn sign_unattested_package(
    kit_dir: &Path,
    zip_path: &Path,
    version: &str,
    min_upgradable: &str,
    base_url: &str,
    expected_channel_url: &str,
    notes_file: &Option<PathBuf>,
    notes_inline: &Option<String>,
    out_path: &Path,
    private_key_file: &Path,
) -> Result<SignReleaseSummary, String> {
    if version.contains('+') {
        return Err("发布版本号不能包含 + 构建元数据，需与服务端路由规则一致。".into());
    }
    let parsed_ver = cst_updater::manifest::version(version)?;
    let parsed_min = cst_updater::manifest::version(min_upgradable)?;
    if parsed_min > parsed_ver {
        return Err("min-upgradable 版本不能高于目标版本。".into());
    }

    let (signing_key, release_key, signing_pub_b64) = load_signing_key(private_key_file)?;
    let signing_pub_fp = release_key.fingerprint();

    // 1. Clean release tree redlines + SFNT + credentials + SHA256SUMS match.
    cst_updater::verify::verify_clean_dist_tree(kit_dir, version)?;

    // 2. Pure-Rust PE32+ x86_64 + WINDOWS_GUI (`Subsystem == 2`) check on all 3 binaries.
    for rel in ["cst-pilot.exe", SERVER_EXE_PATH, UPDATER_EXE_PATH] {
        let bin_bytes = fs::read(kit_dir.join(rel)).map_err(|e| format!("无法读取 {rel}：{e}"))?;
        verify_pe64_windows_gui_binary(&bin_bytes, rel)?;
    }

    // 3. Strict `BUILD-INFO.json` validation.
    let build_info = read_and_validate_build_info(kit_dir, version, &signing_pub_fp)?;

    // 4. Static `.rdata` trust-root verification (channel URL + public key) without executing binaries.
    verify_static_binary_update_root(kit_dir, expected_channel_url, &signing_pub_b64)?;

    // 5. Sums + ZIP inspection + dual Ed25519 signature + offline `VerifiedPackage` staging.
    let sums_bytes = fs::read(kit_dir.join(SHA256SUMS_PATH))
        .map_err(|e| format!("无法读取 {SHA256SUMS_PATH}：{e}"))?;
    let sha256_sums = String::from_utf8(sums_bytes)
        .map_err(|_| format!("{SHA256SUMS_PATH} 必须为 UTF-8 编码。"))?;
    let release_notes = read_bounded_release_notes(notes_file, notes_inline)?;
    let (zip_bytes, archive_size, archive_sha256, unpacked_size) =
        read_and_inspect_zip(zip_path, &sha256_sums)?;

    emit_signed_manifest(
        &signing_key,
        &release_key,
        version,
        &build_info.built_at,
        min_upgradable,
        base_url,
        release_notes,
        sha256_sums,
        &zip_bytes,
        archive_size,
        unpacked_size,
        archive_sha256,
        out_path,
    )
}

async fn read_bounded_response(
    mut resp: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, String> {
    if resp.content_length().is_some_and(|n| n > limit as u64) {
        return Err(format!("响应体超过 {limit} 字节上限。"));
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        if out.len().saturating_add(chunk.len()) > limit {
            return Err(format!("响应体超过 {limit} 字节上限。"));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

pub fn write_zip_entries_unpacked_size(
    kit_dir: &Path,
    rel_paths: &[String],
) -> Result<u64, String> {
    let mut total = 0u64;
    for rel in rel_paths {
        let len = fs::metadata(kit_dir.join(rel))
            .map_err(|e| format!("读取发行文件大小失败 {rel}：{e}"))?
            .len();
        total = total.checked_add(len).ok_or("发行目录总字节数溢出。")?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs::File;
    use tokio::net::TcpListener;
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    fn mock_sfnt(magic: &[u8; 4]) -> Vec<u8> {
        let tables = [*b"cmap", *b"name"];
        let header_len = 12 + tables.len() * 16;
        let mut buf = vec![0u8; header_len + 8];
        buf[..4].copy_from_slice(magic);
        buf[4..6].copy_from_slice(&(tables.len() as u16).to_be_bytes());
        for (idx, tag) in tables.iter().enumerate() {
            let entry = 12 + idx * 16;
            buf[entry..entry + 4].copy_from_slice(tag);
            let offset = (header_len + idx * 4) as u32;
            buf[entry + 8..entry + 12].copy_from_slice(&offset.to_be_bytes());
            buf[entry + 12..entry + 16].copy_from_slice(&4u32.to_be_bytes());
        }
        buf
    }

    fn mock_pe64_gui_with_marker(subsystem: u16, marker: &[u8]) -> Vec<u8> {
        let pe_offset = 0x80usize;
        let coff_offset = pe_offset + 4;
        let opt_offset = coff_offset + 20;
        let opt_size = 240usize;
        let mut buf = vec![0u8; opt_offset + opt_size + 64 + marker.len()];
        buf[0..2].copy_from_slice(b"MZ");
        buf[0x3c..0x40].copy_from_slice(&(pe_offset as u32).to_le_bytes());
        buf[pe_offset..coff_offset].copy_from_slice(b"PE\0\0");
        buf[coff_offset..coff_offset + 2].copy_from_slice(&0x8664u16.to_le_bytes());
        buf[coff_offset + 16..coff_offset + 18].copy_from_slice(&(opt_size as u16).to_le_bytes());
        buf[opt_offset..opt_offset + 2].copy_from_slice(&0x020bu16.to_le_bytes());
        buf[opt_offset + 68..opt_offset + 70].copy_from_slice(&subsystem.to_le_bytes());
        let tail_pos = opt_offset + opt_size + 16;
        buf[tail_pos..tail_pos + marker.len()].copy_from_slice(marker);
        buf
    }

    const TEST_COMMIT_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    #[allow(clippy::too_many_arguments)]
    fn build_synthetic_dist_with_channel(
        root: &Path,
        version: &str,
        pub_b64: &str,
        pub_fp: &str,
        manifest_url: &str,
        commit_sha: &str,
        build_info_json: Option<&str>,
    ) -> (PathBuf, PathBuf, PathBuf, String, u64) {
        let dist_dir = root.join("cst-pilot");
        let _ = fs::remove_dir_all(&dist_dir);
        let built_at = "2026-11-26T12:00:00Z";
        let build_info = build_info_json.map(str::to_owned).unwrap_or_else(|| {
            format!(
                "{{\"version\":\"{version}\",\"builtAt\":\"{built_at}\",\"platform\":\"windows-x86_64\",\"embedWeb\":true,\"updateEnabled\":true,\"updatePublicKeyFingerprint\":\"{pub_fp}\"}}\n"
            )
        });
        let marker = format_update_root_marker(manifest_url, pub_b64);
        let main_bin = mock_pe64_gui_with_marker(2, b"main-gui");
        let server_bin = mock_pe64_gui_with_marker(2, &marker);
        let updater_bin = mock_pe64_gui_with_marker(2, &marker);
        let mut files = BTreeMap::from([
            ("cst-pilot.exe".to_owned(), main_bin),
            ("app/cst-server.exe".into(), server_bin.clone()),
            ("app/cst-updater.exe".into(), updater_bin.clone()),
            (
                "VERSION".into(),
                format!("cst-pilot {version}\n").into_bytes(),
            ),
            ("app/BUILD-INFO.json".into(), build_info.into_bytes()),
            ("LICENSE".into(), b"MIT".to_vec()),
            ("THIRD-PARTY-NOTICES.md".into(), b"notices".to_vec()),
            ("app/licenses/mock.txt".into(), b"mock".to_vec()),
            (
                "app/fonts/SourceHanSansCN-VF.otf".into(),
                mock_sfnt(b"OTTO"),
            ),
            ("app/fonts/LICENSE.txt".into(), b"SIL OFL 1.1".to_vec()),
            (
                "app/fonts/JetBrainsMono-Regular.ttf".into(),
                mock_sfnt(&[0, 1, 0, 0]),
            ),
            (
                "app/fonts/JetBrainsMono-Medium.ttf".into(),
                mock_sfnt(&[0, 1, 0, 0]),
            ),
            (
                "app/fonts/JetBrainsMono-OFL.txt".into(),
                b"SIL OFL".to_vec(),
            ),
            ("app/prompts/APPEND_SYSTEM.md".into(), b"prompt".to_vec()),
            ("app/telemetry/timserver_1.crt".into(), b"cert".to_vec()),
            ("app/telemetry/currency.json".into(), b"{}".to_vec()),
            ("app/defaults/settings.json".into(), b"{}".to_vec()),
            ("app/defaults/models-store.json".into(), b"{}".to_vec()),
            ("app/defaults/telemetry.json".into(), b"{}".to_vec()),
            ("app/defaults/web-search.json".into(), b"{}".to_vec()),
        ]);
        for skill in cst_updater::layout::REQUIRED_SKILL_NAMES {
            files.insert(format!("app/skills/{skill}/SKILL.md"), b"skill".to_vec());
        }
        let mut sums = String::new();
        for (rel, bytes) in &files {
            let p = dist_dir.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, bytes).unwrap();
            sums.push_str(&format!("{:x}  {rel}\n", Sha256::digest(bytes)));
        }
        fs::write(dist_dir.join(SHA256SUMS_PATH), sums.as_bytes()).unwrap();

        let archive_filename = format!("cst-pilot-{version}-windows-x64.zip");
        let zip_path = root.join(&archive_filename);
        let mut zip = ZipWriter::new(File::create(&zip_path).unwrap());
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let mut unpacked = 0u64;
        for (rel, bytes) in &files {
            zip.start_file(format!("cst-pilot/{rel}"), opts).unwrap();
            zip.write_all(bytes).unwrap();
            unpacked += bytes.len() as u64;
        }
        zip.start_file(format!("cst-pilot/{SHA256SUMS_PATH}"), opts)
            .unwrap();
        zip.write_all(sums.as_bytes()).unwrap();
        unpacked += sums.len() as u64;
        zip.finish().unwrap();

        let zip_bytes = fs::read(&zip_path).unwrap();
        let attestation_path =
            root.join(format!("cst-pilot-{version}-windows-x64.attestation.json"));
        let attestation = BuildAttestation {
            schema_version: 1,
            version: version.to_owned(),
            platform: "windows-x86_64".to_owned(),
            built_at: built_at.to_owned(),
            git_commit: commit_sha.to_owned(),
            update_enabled: true,
            manifest_url: manifest_url.to_owned(),
            update_public_key_fingerprint: pub_fp.to_owned(),
            verified_key_probe: true,
            archive_filename,
            archive_size: zip_bytes.len() as u64,
            archive_sha256: format!("{:x}", Sha256::digest(&zip_bytes)),
            unpacked_size: unpacked,
            sha256_sums_sha256: format!("{:x}", Sha256::digest(sums.as_bytes())),
            updater_exe_sha256: format!("{:x}", Sha256::digest(&updater_bin)),
            server_exe_sha256: format!("{:x}", Sha256::digest(&server_bin)),
        };
        let att_sha256 = write_build_attestation(&attestation_path, &attestation).unwrap();

        (dist_dir, zip_path, attestation_path, att_sha256, unpacked)
    }

    fn build_synthetic_dist(
        root: &Path,
        version: &str,
        pub_b64: &str,
        pub_fp: &str,
        commit_sha: &str,
        build_info_json: Option<&str>,
    ) -> (PathBuf, PathBuf, PathBuf, String, u64) {
        build_synthetic_dist_with_channel(
            root,
            version,
            pub_b64,
            pub_fp,
            OFFICIAL_MANIFEST_URL,
            commit_sha,
            build_info_json,
        )
    }

    #[test]
    fn resolve_update_build_config_defaults_to_official_and_respects_channels() {
        let default_cfg = resolve_update_build_config_from_values(None, false, None, None)
            .unwrap()
            .expect("default dist must enable official stable update config");
        assert_eq!(default_cfg.manifest_url, OFFICIAL_MANIFEST_URL);
        assert_eq!(default_cfg.public_key_b64, OFFICIAL_PUBLIC_KEY_B64);
        assert_eq!(default_cfg.public_key_fingerprint, "146afb5bf759af97");

        let beta_cfg = resolve_update_build_config_from_values(
            Some(DistChannel::Beta),
            true,
            Some(OFFICIAL_MANIFEST_URL),
            None,
        )
        .unwrap()
        .expect("explicit --channel beta overrides ambient stable env url");
        assert_eq!(beta_cfg.manifest_url, OFFICIAL_BETA_MANIFEST_URL);

        assert!(
            resolve_update_build_config_from_values(Some(DistChannel::Disabled), false, None, None)
                .unwrap()
                .is_none()
        );
        assert!(
            resolve_update_build_config_from_values(Some(DistChannel::Disabled), true, None, None)
                .is_err()
        );
        assert!(
            resolve_update_build_config_from_values(None, false, Some("disabled"), None)
                .unwrap()
                .is_none()
        );
        assert!(
            resolve_update_build_config_from_values(None, true, Some("disabled"), None).is_err()
        );
    }

    #[test]
    fn pe64_gui_and_ssh_host_validators_enforce_strict_boundaries() {
        let gui_pe = mock_pe64_gui_with_marker(2, b"test");
        assert!(verify_pe64_windows_gui_binary(&gui_pe, "cst-pilot.exe").is_ok());

        let console_pe = mock_pe64_gui_with_marker(3, b"test");
        assert!(verify_pe64_windows_gui_binary(&console_pe, "cst-pilot.exe").is_err());
        assert!(verify_pe64_windows_gui_binary(b"not-a-pe", "cst-pilot.exe").is_err());

        let stable_m = format_update_root_marker(OFFICIAL_MANIFEST_URL, OFFICIAL_PUBLIC_KEY_B64);
        let beta_m = format_update_root_marker(OFFICIAL_BETA_MANIFEST_URL, OFFICIAL_PUBLIC_KEY_B64);
        let mut dual = stable_m.clone();
        dual.extend_from_slice(&beta_m);
        assert!(
            verify_exact_single_root_marker(
                &stable_m,
                &stable_m,
                "test.exe",
                OFFICIAL_MANIFEST_URL
            )
            .is_ok()
        );
        assert!(
            verify_exact_single_root_marker(&dual, &stable_m, "test.exe", OFFICIAL_MANIFEST_URL)
                .is_err()
        );

        assert_eq!(validate_ssh_host("cstoa").unwrap(), "cstoa");
        assert_eq!(
            validate_ssh_host("deploy@example.org").unwrap(),
            "deploy@example.org"
        );
        assert!(validate_ssh_host("-oProxyCommand=evil").is_err());
        assert!(validate_ssh_host("cstoa;id").is_err());
        assert!(validate_ssh_host("").is_err());
    }

    #[test]
    fn keygen_creates_valid_key_and_refuses_overwrite() {
        let dir = env::temp_dir().join(format!("xtask-keygen-{}", Uuid::new_v4()));
        let key_file = dir.join("release.key");
        let (pub_b64, fp) = generate_keypair_file(&key_file).unwrap();
        assert_eq!(pub_b64.len(), 44);
        assert_eq!(fp.len(), 16);
        assert!(parse_canonical_public_key(&pub_b64).is_ok());
        assert!(generate_keypair_file(&key_file).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sign_release_supports_both_dist_dir_and_zip_only_and_self_verifies() {
        let dir = env::temp_dir().join(format!("xtask-sign-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let key_file = dir.join("release.key");
        let (pub_b64, fp) = generate_keypair_file(&key_file).unwrap();
        let (dist_dir, zip_path, attestation_path, att_sha256, expected_unpacked) =
            build_synthetic_dist(&dir, "v0.9.8", &pub_b64, &fp, TEST_COMMIT_SHA, None);
        let out_manifest = dir.join("manifest.signed.json");

        // 1. With explicit `--dist-dir`
        let summary = sign_release(&SignReleaseArgs {
            dist_dir: Some(dist_dir),
            zip_path: zip_path.clone(),
            attestation_path: attestation_path.clone(),
            expected_attestation_sha256: att_sha256.clone(),
            expected_commit: TEST_COMMIT_SHA.into(),
            version: Some("v0.9.8".into()),
            min_upgradable: "v0.9.0".into(),
            base_url: DEFAULT_PUBLIC_BASE_URL.into(),
            notes_file: None,
            notes_inline: Some("test notes".into()),
            out_path: out_manifest.clone(),
            private_key_file: key_file.clone(),
        })
        .unwrap();

        assert_eq!(summary.unpacked_size, expected_unpacked);
        assert_eq!(
            summary.archive_url,
            "https://www.cstoa.top/api/agent/releases/files/v0.9.8/cst-pilot-v0.9.8-windows-x64.zip"
        );
        let (key, _, _) = parse_canonical_public_key(&pub_b64).unwrap();
        let verified = key.verify(&fs::read(&out_manifest).unwrap()).unwrap();
        assert_eq!(verified.info().archive.unpacked_size, expected_unpacked);

        // 2. With `--dist-dir` omitted (ZIP-only extraction & verification)
        let out_manifest_zip_only = dir.join("manifest-zip-only.signed.json");
        let summary2 = sign_release(&SignReleaseArgs {
            dist_dir: None,
            zip_path,
            attestation_path,
            expected_attestation_sha256: att_sha256,
            expected_commit: TEST_COMMIT_SHA.into(),
            version: None,
            min_upgradable: "v0.9.0".into(),
            base_url: DEFAULT_PUBLIC_BASE_URL.into(),
            notes_file: None,
            notes_inline: Some("zip only".into()),
            out_path: out_manifest_zip_only.clone(),
            private_key_file: key_file,
        })
        .unwrap();
        assert_eq!(summary2.version, "v0.9.8");
        assert_eq!(summary2.unpacked_size, expected_unpacked);
        assert!(
            key.verify(&fs::read(&out_manifest_zip_only).unwrap())
                .is_ok()
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sign_release_rejects_mismatched_attestation_hash_wrong_commit_or_tampered_artifacts() {
        let dir = env::temp_dir().join(format!("xtask-sign-bind-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let key_file = dir.join("release.key");
        let (pub_b64, fp) = generate_keypair_file(&key_file).unwrap();
        let out_manifest = dir.join("manifest.signed.json");

        // 1. Missing updatePublicKeyFingerprint in BUILD-INFO.json must fail.
        let (dist_dir, zip_path, attestation_path, att_sha256, _) = build_synthetic_dist(
            &dir,
            "v0.9.8",
            &pub_b64,
            &fp,
            TEST_COMMIT_SHA,
            Some(
                "{\"version\":\"v0.9.8\",\"builtAt\":\"2026-11-26T12:00:00Z\",\"updateEnabled\":true}\n",
            ),
        );
        let err = sign_release(&SignReleaseArgs {
            dist_dir: Some(dist_dir),
            zip_path,
            attestation_path,
            expected_attestation_sha256: att_sha256,
            expected_commit: TEST_COMMIT_SHA.into(),
            version: Some("v0.9.8".into()),
            min_upgradable: "v0.9.0".into(),
            base_url: DEFAULT_PUBLIC_BASE_URL.into(),
            notes_file: None,
            notes_inline: None,
            out_path: out_manifest.clone(),
            private_key_file: key_file.clone(),
        })
        .err()
        .expect("missing fingerprint must fail");
        assert!(err.contains("updatePublicKeyFingerprint"));

        // 2. Swapping attestation + artifacts together without matching out-of-band SHA-256 must fail.
        let (dist_dir, zip_path, attestation_path, att_sha256, _) =
            build_synthetic_dist(&dir, "v0.9.8", &pub_b64, &fp, TEST_COMMIT_SHA, None);
        let err = sign_release(&SignReleaseArgs {
            dist_dir: Some(dist_dir.clone()),
            zip_path: zip_path.clone(),
            attestation_path: attestation_path.clone(),
            expected_attestation_sha256: "e".repeat(64),
            expected_commit: TEST_COMMIT_SHA.into(),
            version: Some("v0.9.8".into()),
            min_upgradable: "v0.9.0".into(),
            base_url: DEFAULT_PUBLIC_BASE_URL.into(),
            notes_file: None,
            notes_inline: None,
            out_path: out_manifest.clone(),
            private_key_file: key_file.clone(),
        })
        .err()
        .expect("mismatched attestation SHA-256 must fail");
        assert!(err.contains("构建证明 SHA-256"));

        // 3. Approved commit mismatch must fail.
        let err = sign_release(&SignReleaseArgs {
            dist_dir: Some(dist_dir.clone()),
            zip_path: zip_path.clone(),
            attestation_path: attestation_path.clone(),
            expected_attestation_sha256: att_sha256.clone(),
            expected_commit: "f".repeat(40),
            version: Some("v0.9.8".into()),
            min_upgradable: "v0.9.0".into(),
            base_url: DEFAULT_PUBLIC_BASE_URL.into(),
            notes_file: None,
            notes_inline: None,
            out_path: out_manifest.clone(),
            private_key_file: key_file.clone(),
        })
        .err()
        .expect("wrong expected commit must fail");
        assert!(err.contains("提交哈希"));

        // 4. verifiedKeyProbe=false must fail even if SHA-256 matches.
        let mut unverified_att =
            read_and_verify_build_attestation(&attestation_path, &att_sha256).unwrap();
        unverified_att.verified_key_probe = false;
        let unverified_sha = write_build_attestation(&attestation_path, &unverified_att).unwrap();
        let err = sign_release(&SignReleaseArgs {
            dist_dir: Some(dist_dir.clone()),
            zip_path: zip_path.clone(),
            attestation_path: attestation_path.clone(),
            expected_attestation_sha256: unverified_sha,
            expected_commit: TEST_COMMIT_SHA.into(),
            version: Some("v0.9.8".into()),
            min_upgradable: "v0.9.0".into(),
            base_url: DEFAULT_PUBLIC_BASE_URL.into(),
            notes_file: None,
            notes_inline: None,
            out_path: out_manifest.clone(),
            private_key_file: key_file.clone(),
        })
        .err()
        .expect("unverified key probe must fail");
        assert!(err.contains("verifiedKeyProbe"));

        // 5. Substituting app/cst-updater.exe after build (mismatching attestation digest) must fail.
        unverified_att.verified_key_probe = true;
        unverified_att.updater_exe_sha256 = "0".repeat(64);
        let tampered_sha = write_build_attestation(&attestation_path, &unverified_att).unwrap();
        let err = sign_release(&SignReleaseArgs {
            dist_dir: Some(dist_dir),
            zip_path,
            attestation_path,
            expected_attestation_sha256: tampered_sha,
            expected_commit: TEST_COMMIT_SHA.into(),
            version: Some("v0.9.8".into()),
            min_upgradable: "v0.9.0".into(),
            base_url: DEFAULT_PUBLIC_BASE_URL.into(),
            notes_file: None,
            notes_inline: None,
            out_path: out_manifest,
            private_key_file: key_file,
        })
        .err()
        .expect("tampered updater digest must fail");
        assert!(err.contains(UPDATER_EXE_PATH));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_probe_command_validates_stdout_and_exit_status() {
        #[cfg(windows)]
        let make_cmd = |script: &str| {
            let mut c = Command::new("cmd.exe");
            c.args(["/C", script]);
            c
        };
        #[cfg(not(windows))]
        let make_cmd = |script: &str| {
            let mut c = Command::new("/bin/sh");
            c.args(["-c", script]);
            c
        };

        assert!(
            verify_probe_command(
                make_cmd("echo 0123456789abcdef"),
                "test.exe",
                "0123456789abcdef"
            )
            .is_ok()
        );
        assert!(
            verify_probe_command(
                make_cmd("echo deadbeefdeadbeef"),
                "test.exe",
                "0123456789abcdef"
            )
            .is_err()
        );
        assert!(verify_probe_command(make_cmd("exit 1"), "test.exe", "0123456789abcdef").is_err());
    }

    #[tokio::test]
    async fn execute_release_supports_attested_bundle_unattested_package_and_channel_guard() {
        let dir = env::temp_dir().join(format!("xtask-exec-release-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let key_file = dir.join("release.key");
        let (pub_b64, fp) = generate_keypair_file(&key_file).unwrap();
        let (dist_dir, zip_path, _att_path, att_sha256, _) =
            build_synthetic_dist(&dir, "v0.9.9", &pub_b64, &fp, TEST_COMMIT_SHA, None);

        // 1. Cross-channel guard: pushing a `stable`-marked package to `beta` must fail in both modes!
        let cross_err = execute_release(
            &ReleaseCommandArgs {
                package_path: dir.clone(),
                min_upgradable: "v0.9.0".into(),
                channel: "beta".into(),
                version: None,
                attestation_path: None,
                expected_attestation_sha256: Some(att_sha256.clone()),
                expected_commit: Some(TEST_COMMIT_SHA.into()),
                unattested_package: false,
                notes_file: None,
                notes_inline: None,
                private_key_file: key_file.clone(),
                out_manifest: None,
                ssh_host: None,
                ssh_remote_port: DEFAULT_SSH_REMOTE_PORT,
                admin_url: None,
                public_base_url: DEFAULT_PUBLIC_BASE_URL.into(),
                public_manifest_url: None,
                public_archive_url: None,
                activate: true,
                dry_run: true,
            },
            None,
        )
        .await
        .expect_err("pushing stable-attested package to beta must fail");
        assert!(cross_err.contains("通道"));

        let cross_unattested_err = execute_release(
            &ReleaseCommandArgs {
                package_path: zip_path.clone(),
                min_upgradable: "v0.9.0".into(),
                channel: "beta".into(),
                version: None,
                attestation_path: None,
                expected_attestation_sha256: None,
                expected_commit: None,
                unattested_package: true,
                notes_file: None,
                notes_inline: None,
                private_key_file: key_file.clone(),
                out_manifest: None,
                ssh_host: None,
                ssh_remote_port: DEFAULT_SSH_REMOTE_PORT,
                admin_url: None,
                public_base_url: DEFAULT_PUBLIC_BASE_URL.into(),
                public_manifest_url: None,
                public_archive_url: None,
                activate: true,
                dry_run: true,
            },
            None,
        )
        .await
        .expect_err("pushing stable binary to beta in unattested mode must fail static root check");
        assert!(cross_unattested_err.contains("静态标记"));

        // 2. Unpacked kit directory dry-run in `--unattested-package` mode (auto-zips & verifies PE64 + root marker).
        let standalone_root = dir.join("standalone-unpacked");
        fs::create_dir_all(&standalone_root).unwrap();
        let copy_kit = standalone_root.join("cst-pilot");
        crate::copy_dir_filtered(&dist_dir, &copy_kit, |_| true).unwrap();
        execute_release(
            &ReleaseCommandArgs {
                package_path: copy_kit,
                min_upgradable: "v0.9.0".into(),
                channel: "stable".into(),
                version: None,
                attestation_path: None,
                expected_attestation_sha256: None,
                expected_commit: None,
                unattested_package: true,
                notes_file: None,
                notes_inline: Some("standalone dir dry run".into()),
                private_key_file: key_file.clone(),
                out_manifest: None,
                ssh_host: None,
                ssh_remote_port: DEFAULT_SSH_REMOTE_PORT,
                admin_url: None,
                public_base_url: DEFAULT_PUBLIC_BASE_URL.into(),
                public_manifest_url: None,
                public_archive_url: None,
                activate: true,
                dry_run: true,
            },
            None,
        )
        .await
        .expect("standalone unpacked kit directory dry-run must succeed");

        // 3. Full attested bundle (`--package <dist_parent_dir>`) upload + public endpoint verification.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let expected_token = "t".repeat(48);
        let server_token = expected_token.clone();

        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut raw = Vec::new();
            let mut buf = [0u8; 8192];
            let mut header_end = None;
            let mut content_len = 0usize;
            loop {
                let n = sock.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..n]);
                if header_end.is_none()
                    && let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n")
                {
                    header_end = Some(pos + 4);
                    let hdr = String::from_utf8_lossy(&raw[..pos]);
                    assert!(
                        hdr.starts_with("POST /v1/admin/releases?channel=stable&activate=true")
                    );
                    assert!(
                        hdr.to_ascii_lowercase()
                            .contains(&format!("authorization: bearer {server_token}"))
                    );
                    for line in hdr.lines() {
                        if let Some((k, v)) = line.split_once(':')
                            && k.trim().eq_ignore_ascii_case("content-length")
                        {
                            content_len = v.trim().parse().unwrap();
                        }
                    }
                }
                if let Some(end) = header_end
                    && raw.len() >= end + content_len
                {
                    break;
                }
            }
            let body = &raw[header_end.unwrap()..header_end.unwrap() + content_len];
            // Extract uploaded manifest part and archive part size from multipart body
            let m_hdr =
                b"filename=\"manifest.signed.json\"\r\nContent-Type: application/json\r\n\r\n";
            let m_start = body.windows(m_hdr.len()).position(|w| w == m_hdr).unwrap() + m_hdr.len();
            let m_end = body[m_start..]
                .windows(4)
                .position(|w| w == b"\r\n--")
                .unwrap()
                + m_start;
            let uploaded_manifest = body[m_start..m_end].to_vec();

            let a_hdr = b"Content-Type: application/zip\r\n\r\n";
            let a_start = body.windows(a_hdr.len()).position(|w| w == a_hdr).unwrap() + a_hdr.len();
            let a_end = body[a_start..]
                .windows(4)
                .rposition(|w| w == b"\r\n--")
                .unwrap()
                + a_start;
            let uploaded_archive_len = a_end - a_start;

            let body_json =
                r#"{"channel":"stable","activated":true,"record":{"version":"v0.9.9"}}"#;
            let resp = format!(
                "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_json}",
                body_json.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            drop(sock);

            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req_buf = [0u8; 2048];
            let _ = sock.read(&mut req_buf).await.unwrap();
            let hdr = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                uploaded_manifest.len()
            );
            sock.write_all(hdr.as_bytes()).await.unwrap();
            sock.write_all(&uploaded_manifest).await.unwrap();
            drop(sock);

            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req_buf = [0u8; 2048];
            let _ = sock.read(&mut req_buf).await.unwrap();
            let hdr = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/zip\r\nContent-Length: {uploaded_archive_len}\r\nConnection: close\r\n\r\n"
            );
            sock.write_all(hdr.as_bytes()).await.unwrap();
        });

        execute_release(
            &ReleaseCommandArgs {
                package_path: dir.clone(),
                min_upgradable: "v0.9.0".into(),
                channel: "stable".into(),
                version: None,
                attestation_path: None,
                expected_attestation_sha256: Some(att_sha256),
                expected_commit: Some(TEST_COMMIT_SHA.into()),
                unattested_package: false,
                notes_file: None,
                notes_inline: Some("one-shot release test".into()),
                private_key_file: key_file,
                out_manifest: None,
                ssh_host: None,
                ssh_remote_port: DEFAULT_SSH_REMOTE_PORT,
                admin_url: Some(format!("http://{addr}/v1/admin/releases")),
                public_base_url: DEFAULT_PUBLIC_BASE_URL.into(),
                public_manifest_url: Some(format!(
                    "http://{addr}/v1/releases/channels/stable/latest"
                )),
                public_archive_url: Some(format!(
                    "http://{addr}/v1/releases/files/v0.9.9/cst-pilot-v0.9.9-windows-x64.zip"
                )),
                activate: true,
                dry_run: false,
            },
            Some(&Zeroizing::new(expected_token)),
        )
        .await
        .unwrap();

        server.await.unwrap();
        let _ = fs::remove_dir_all(&dir);
    }
}

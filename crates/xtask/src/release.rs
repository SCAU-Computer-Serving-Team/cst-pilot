use std::env;
use std::fs::{self, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use chrono::{SecondsFormat, Utc};
use cst_updater::layout::{BUILD_INFO_PATH, SHA256SUMS_PATH};
use cst_updater::manifest::{
    Archive, MANIFEST_DOMAIN, ReleaseKey, ReleaseManifest, SUMS_DOMAIN, SignedManifest,
    validate_release_url,
};
#[cfg(windows)]
use cst_updater::verify::VerifiedPackage;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

pub const DEFAULT_PUBLIC_BASE_URL: &str = "https://www.cstoa.top/api/agent/releases";
pub const DEFAULT_ADMIN_URL: &str = "http://127.0.0.1:18787/v1/admin/releases";
pub const ENV_MANIFEST_URL: &str = "CST_UPDATE_MANIFEST_URL";
pub const ENV_PUBLIC_KEY: &str = "CST_UPDATE_PUBLIC_KEY";
pub const ENV_PRIVATE_KEY_FILE: &str = "CST_RELEASE_PRIVATE_KEY_FILE";
pub const ENV_ADMIN_TOKEN: &str = "CST_RELEASE_ADMIN_TOKEN";

#[derive(Clone, Debug)]
pub struct UpdateBuildConfig {
    pub manifest_url: String,
    pub public_key_b64: String,
    pub public_key_fingerprint: String,
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

pub fn resolve_update_build_config(require: bool) -> Result<Option<UpdateBuildConfig>, String> {
    let manifest_url = env::var(ENV_MANIFEST_URL)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let public_key = env::var(ENV_PUBLIC_KEY)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());

    match (manifest_url, public_key) {
        (None, None) => {
            if require {
                Err(format!(
                    "启用 --require-update-config 时必须同时设置 {ENV_MANIFEST_URL} 与 {ENV_PUBLIC_KEY}。"
                ))
            } else {
                Ok(None)
            }
        }
        (Some(_), None) | (None, Some(_)) => Err(format!(
            "{ENV_MANIFEST_URL} 与 {ENV_PUBLIC_KEY} 必须同时配置或同时留空。"
        )),
        (Some(url), Some(key_b64)) => {
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

fn load_signing_key(path: &Path) -> Result<(SigningKey, ReleaseKey), String> {
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
    let release_key = ReleaseKey::from_bytes(signing.verifying_key().as_bytes())?;
    Ok((signing, release_key))
}

pub struct SignReleaseArgs {
    pub dist_dir: PathBuf,
    pub zip_path: PathBuf,
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

    let dist_dir = dist_dir.ok_or("sign-release 必须指定 --dist-dir <dir>。")?;
    let zip_path = zip_path.ok_or("sign-release 必须指定 --zip <path>。")?;
    let min_upgradable =
        min_upgradable.ok_or("sign-release 必须指定 --min-upgradable <vX.Y.Z>。")?;
    let out_path = out_path.ok_or("sign-release 必须指定 --out <manifest.signed.json>。")?;
    let private_key_file = match private_key_file {
        Some(p) => p,
        None => env::var(ENV_PRIVATE_KEY_FILE)
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| {
                format!(
                    "必须通过 --private-key-file 或环境变量 {ENV_PRIVATE_KEY_FILE} 指定私钥文件。"
                )
            })?,
    };
    if notes_file.is_some() && notes_inline.is_some() {
        return Err("--notes-file 与 --notes 不能同时指定。".into());
    }

    Ok(SignReleaseArgs {
        dist_dir,
        zip_path,
        version,
        min_upgradable,
        base_url,
        notes_file,
        notes_inline,
        out_path,
        private_key_file,
    })
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

pub fn sign_release(args: &SignReleaseArgs) -> Result<SignReleaseSummary, String> {
    let version = match &args.version {
        Some(v) => v.clone(),
        None => cst_updater::layout::read_version(&args.dist_dir)?,
    };
    let parsed_ver = cst_updater::manifest::version(&version)?;
    let parsed_min = cst_updater::manifest::version(&args.min_upgradable)?;
    if parsed_min > parsed_ver {
        return Err("min-upgradable 版本不能高于目标版本。".into());
    }

    let base_url = args.base_url.trim().trim_end_matches('/');
    let parsed_base = validate_release_url(base_url)?;
    if parsed_base.query().is_some() || parsed_base.fragment().is_some() {
        return Err("--base-url 不允许包含查询参数或片段。".into());
    }

    let (signing_key, release_key) = load_signing_key(&args.private_key_file)?;
    cst_updater::verify::verify_clean_dist_tree(&args.dist_dir, &version)?;

    let sums_bytes = fs::read(args.dist_dir.join(SHA256SUMS_PATH))
        .map_err(|e| format!("无法读取 {SHA256SUMS_PATH}：{e}"))?;
    let sha256_sums = String::from_utf8(sums_bytes)
        .map_err(|_| format!("{SHA256SUMS_PATH} 必须为 UTF-8 编码。"))?;

    let build_info_bytes = fs::read(args.dist_dir.join(BUILD_INFO_PATH))
        .map_err(|e| format!("无法读取 {BUILD_INFO_PATH}：{e}"))?;
    let build_info: serde_json::Value =
        serde_json::from_slice(&build_info_bytes).map_err(|_| "BUILD-INFO.json 格式无效。")?;
    let built_at = build_info
        .get("builtAt")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true));
    chrono::DateTime::parse_from_rfc3339(&built_at)
        .map_err(|_| "BUILD-INFO.json 中的 builtAt 不是有效的 RFC 3339 时间戳。")?;

    let release_notes = if let Some(ref notes_path) = args.notes_file {
        fs::read_to_string(notes_path)
            .map_err(|e| format!("无法读取更新说明 {}：{e}", notes_path.display()))?
    } else {
        args.notes_inline.clone().unwrap_or_default()
    };
    if release_notes.len() > 16 * 1024 {
        return Err("更新说明超过 16 KiB 上限。".into());
    }

    let zip_bytes = fs::read(&args.zip_path)
        .map_err(|e| format!("无法读取发行 ZIP {}：{e}", args.zip_path.display()))?;
    let archive_size = zip_bytes.len() as u64;
    let archive_sha256 = format!("{:x}", Sha256::digest(&zip_bytes));

    let unpacked_size = inspect_zip_unpacked_size(&zip_bytes, &sha256_sums)?;
    let archive_filename = format!("cst-pilot-{version}-windows-x64.zip");
    let archive_url = format!("{base_url}/files/{version}/{archive_filename}");
    validate_release_url(&archive_url)?;

    let sha256_signature = sign_domain(&signing_key, SUMS_DOMAIN, sha256_sums.as_bytes());
    let manifest = ReleaseManifest {
        schema_version: 1,
        platform: "windows-x86_64".to_owned(),
        version: version.clone(),
        built_at,
        min_upgradable_version: args.min_upgradable.clone(),
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
    let signature = sign_domain(&signing_key, MANIFEST_DOMAIN, payload.as_bytes());
    let signed = SignedManifest { payload, signature };
    let mut envelope_bytes =
        serde_json::to_vec(&signed).map_err(|_| "SignedManifest 序列化失败。")?;
    envelope_bytes.push(b'\n');

    // Offline closed-loop verification using the exact `cst-updater` verifier.
    let verified = release_key.verify(&envelope_bytes)?;
    verify_staged_offline(&verified, &zip_bytes)?;

    if let Some(parent) = args.out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|e| format!("无法创建清单输出目录 {}：{e}", parent.display()))?;
    }
    fs::write(&args.out_path, &envelope_bytes)
        .map_err(|e| format!("无法写入签名清单 {}：{e}", args.out_path.display()))?;

    Ok(SignReleaseSummary {
        version,
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

fn inspect_zip_unpacked_size(zip_bytes: &[u8], expected_sums: &str) -> Result<u64, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes))
        .map_err(|e| format!("发行 ZIP 格式无效：{e}"))?;
    let mut unpacked_size = 0u64;
    let mut saw_sums = false;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("读取发行 ZIP 条目失败：{e}"))?;
        if entry.is_dir() {
            continue;
        }
        unpacked_size = unpacked_size
            .checked_add(entry.size())
            .ok_or("发行 ZIP 解压总字节数溢出。")?;
        if entry.name() == format!("cst-pilot/{SHA256SUMS_PATH}") {
            let mut inner = Vec::new();
            entry
                .read_to_end(&mut inner)
                .map_err(|e| format!("读取 ZIP 内 {SHA256SUMS_PATH} 失败：{e}"))?;
            if inner != expected_sums.as_bytes() {
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
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp_root = env::temp_dir().join(format!("cst-sign-verify-{}-{stamp}", std::process::id()));
    fs::create_dir_all(&temp_root).map_err(|e| format!("无法创建离线验签临时目录：{e}"))?;
    let cancel = CancellationToken::new();
    #[cfg(windows)]
    let res =
        VerifiedPackage::stage_from_archive_bytes(&temp_root, verified.clone(), zip_bytes, &cancel)
            .map(drop);
    #[cfg(not(windows))]
    let res = (|| -> Result<(), String> {
        let archive_path = temp_root.join("package.zip");
        let program_dir = temp_root.join("program");
        fs::write(&archive_path, zip_bytes).map_err(|e| format!("无法写入离线验签压缩包：{e}"))?;
        fs::create_dir_all(&program_dir).map_err(|e| format!("无法创建离线验签解压目录：{e}"))?;
        cst_updater::verify::extract_archive(&archive_path, &program_dir, verified, &cancel)?;
        cst_updater::verify::verify_tree(&program_dir, verified, &cancel)
    })();
    let _ = fs::remove_dir_all(&temp_root);
    res
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
    let public_key_b64 = match public_key_b64 {
        Some(v) => v,
        None => env::var(ENV_PUBLIC_KEY)
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                format!("必须通过 --public-key 或环境变量 {ENV_PUBLIC_KEY} 指定官方升级公钥。")
            })?,
    };

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

pub fn run_publish_release(raw_args: impl Iterator<Item = String>) -> Result<(), String> {
    let args = parse_publish_release_args(raw_args)?;
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
    let resp = client
        .post(upload_url)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", admin_token.as_str()),
        )
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
    let resp_bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("读取发布响应失败：{e}"))?;
    if status != reqwest::StatusCode::CREATED {
        let detail = String::from_utf8_lossy(&resp_bytes);
        return Err(format!(
            "服务端拒绝发布（HTTP {}）：{detail}",
            status.as_u16()
        ));
    }
    println!(
        "[xtask] Upload accepted (201 Created): {}",
        String::from_utf8_lossy(&resp_bytes).trim()
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
        let remote_manifest = manifest_resp
            .bytes()
            .await
            .map_err(|e| format!("读取公开清单正文失败：{e}"))?;
        let remote_verified = release_key
            .verify(&remote_manifest)
            .map_err(|e| format!("发布后公开清单验签失败：{e}"))?;
        if remote_verified.info().version != verified.info().version
            || remote_manifest.as_ref() != manifest_bytes.as_slice()
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
    use tokio::io::AsyncWriteExt;
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

    fn build_synthetic_dist(root: &Path, version: &str) -> (PathBuf, PathBuf, u64) {
        let dist_dir = root.join("cst-pilot");
        let mut files = BTreeMap::from([
            ("cst-pilot.exe".to_owned(), b"synthetic main".to_vec()),
            ("app/cst-server.exe".into(), b"synthetic server".to_vec()),
            ("app/cst-updater.exe".into(), b"synthetic updater".to_vec()),
            (
                "VERSION".into(),
                format!("cst-pilot {version}\n").into_bytes(),
            ),
            (
                "app/BUILD-INFO.json".into(),
                format!("{{\"version\":\"{version}\",\"builtAt\":\"2026-11-26T12:00:00Z\"}}\n")
                    .into_bytes(),
            ),
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

        let zip_path = root.join(format!("cst-pilot-{version}-windows-x64.zip"));
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

        (dist_dir, zip_path, unpacked)
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
    fn sign_release_includes_sha256sums_in_unpacked_size_and_self_verifies() {
        let dir = env::temp_dir().join(format!("xtask-sign-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let key_file = dir.join("release.key");
        let (pub_b64, _) = generate_keypair_file(&key_file).unwrap();
        let (dist_dir, zip_path, expected_unpacked) = build_synthetic_dist(&dir, "v0.9.8");
        let out_manifest = dir.join("manifest.signed.json");

        let summary = sign_release(&SignReleaseArgs {
            dist_dir,
            zip_path,
            version: Some("v0.9.8".into()),
            min_upgradable: "v0.9.0".into(),
            base_url: DEFAULT_PUBLIC_BASE_URL.into(),
            notes_file: None,
            notes_inline: Some("test notes".into()),
            out_path: out_manifest.clone(),
            private_key_file: key_file,
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
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn publish_release_streams_multipart_and_verifies_public_endpoints() {
        let dir = env::temp_dir().join(format!("xtask-publish-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let key_file = dir.join("release.key");
        let (pub_b64, _) = generate_keypair_file(&key_file).unwrap();
        let (dist_dir, zip_path, _) = build_synthetic_dist(&dir, "v0.9.9");
        let out_manifest = dir.join("manifest.signed.json");
        let summary = sign_release(&SignReleaseArgs {
            dist_dir,
            zip_path: zip_path.clone(),
            version: Some("v0.9.9".into()),
            min_upgradable: "v0.9.0".into(),
            base_url: DEFAULT_PUBLIC_BASE_URL.into(),
            notes_file: None,
            notes_inline: Some("publish test".into()),
            out_path: out_manifest.clone(),
            private_key_file: key_file,
        })
        .unwrap();
        let manifest_bytes = fs::read(&out_manifest).unwrap();
        let archive_size = summary.archive_size;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let expected_token = "t".repeat(40);
        let server_token = expected_token.clone();
        let server_manifest = manifest_bytes.clone();

        let server = tokio::spawn(async move {
            // 1. POST /v1/admin/releases
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
            let manifest_pos = body
                .windows(15)
                .position(|w| w == b"name=\"manifest\"")
                .unwrap();
            let archive_pos = body
                .windows(14)
                .position(|w| w == b"name=\"archive\"")
                .unwrap();
            assert!(manifest_pos < archive_pos);
            let resp = b"HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}";
            sock.write_all(resp).await.unwrap();
            drop(sock);

            // 2. GET /v1/releases/channels/stable/latest
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req_buf = [0u8; 2048];
            let n = sock.read(&mut req_buf).await.unwrap();
            let req = String::from_utf8_lossy(&req_buf[..n]);
            assert!(req.starts_with("GET /v1/releases/channels/stable/latest "));
            let hdr = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                server_manifest.len()
            );
            sock.write_all(hdr.as_bytes()).await.unwrap();
            sock.write_all(&server_manifest).await.unwrap();
            drop(sock);

            // 3. HEAD /v1/releases/files/...
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req_buf = [0u8; 2048];
            let n = sock.read(&mut req_buf).await.unwrap();
            let req = String::from_utf8_lossy(&req_buf[..n]);
            assert!(
                req.starts_with("HEAD /v1/releases/files/v0.9.9/cst-pilot-v0.9.9-windows-x64.zip ")
            );
            let hdr = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/zip\r\nContent-Length: {archive_size}\r\nConnection: close\r\n\r\n"
            );
            sock.write_all(hdr.as_bytes()).await.unwrap();
        });

        publish_release(
            &PublishReleaseArgs {
                manifest_path: out_manifest,
                zip_path,
                channel: "stable".into(),
                activate: true,
                admin_url: format!("http://{addr}/v1/admin/releases"),
                public_base_url: DEFAULT_PUBLIC_BASE_URL.into(),
                public_manifest_url: Some(format!(
                    "http://{addr}/v1/releases/channels/stable/latest"
                )),
                public_archive_url: Some(format!(
                    "http://{addr}/v1/releases/files/v0.9.9/cst-pilot-v0.9.9-windows-x64.zip"
                )),
                public_key_b64: pub_b64,
            },
            &Zeroizing::new(expected_token),
        )
        .await
        .unwrap();

        server.await.unwrap();
        let _ = fs::remove_dir_all(&dir);
    }
}

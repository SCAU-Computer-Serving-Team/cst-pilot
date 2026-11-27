mod release;

use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{SecondsFormat, Utc};
use cst_updater::layout::{
    BUILD_INFO_PATH, DEFAULT_CONFIG_FILES, REQUIRED_FONT_FILES, REQUIRED_SKILL_NAMES,
    SERVER_EXE_PATH, SHA256SUMS_PATH, UPDATER_EXE_PATH,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

fn main() -> ExitCode {
    if let Err(err) = run() {
        eprintln!("[xtask] ERROR: {err}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let Some(cmd) = args.next() else {
        print_usage();
        return Err(
            "缺少子命令（支持 dist / verify-tree / keygen / sign-release / publish-release）。"
                .into(),
        );
    };
    match cmd.as_str() {
        "dist" => run_dist(args),
        "verify-tree" => {
            let dir = args.next().ok_or("verify-tree 需要目标目录路径。")?;
            let version = args.next();
            let dir = PathBuf::from(dir);
            let target_version = match version {
                Some(v) => v,
                None => cst_updater::layout::read_version(&dir)?,
            };
            let count = cst_updater::verify::verify_clean_dist_tree(&dir, &target_version)?;
            println!("[xtask] verify-tree PASS: {count} files validated for {target_version}");
            Ok(())
        }
        "keygen" => release::run_keygen(args),
        "sign-release" => release::run_sign_release(args),
        "publish-release" => release::run_publish_release(args),
        "-h" | "--help" | "help" => {
            print_usage();
            Ok(())
        }
        other => {
            print_usage();
            Err(format!("未知子命令：{other}"))
        }
    }
}

fn print_usage() {
    eprintln!(
        "Usage:\n  cargo run -p xtask -- dist --out <dir> [--version <vX.Y.Z>] [--profile release|dist] [--require-update-config] [--skip-web-build] [--skip-cargo-build] [--skip-smoke] [--no-zip]\n  cargo run -p xtask -- verify-tree <dir> [version]\n  cargo run -p xtask -- keygen --out <key-file>\n  cargo run -p xtask -- sign-release --dist-dir <dir> --zip <path> --attestation <path> --expected-commit <40-hex> --min-upgradable <vX.Y.Z> --out <manifest.signed.json> [--attestation-public-key <base64>] [--version <vX.Y.Z>] [--base-url <url>] [--notes-file <md> | --notes <text>] [--private-key-file <path>]\n  cargo run -p xtask -- publish-release --manifest <path> --zip <path> [--channel stable|beta] [--no-activate] [--admin-url <url>] [--public-base-url <url>] [--public-manifest-url <url>] [--public-archive-url <url>] [--public-key <base64>]"
    );
}

struct DistArgs {
    out_dir: PathBuf,
    version: String,
    profile: String,
    require_update_config: bool,
    skip_web_build: bool,
    skip_cargo_build: bool,
    skip_smoke: bool,
    make_zip: bool,
}

fn parse_dist_args(mut args: impl Iterator<Item = String>) -> Result<DistArgs, String> {
    let mut out_dir: Option<PathBuf> = None;
    let mut version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let mut profile = "release".to_owned();
    let mut require_update_config = false;
    let mut skip_web_build = false;
    let mut skip_cargo_build = false;
    let mut skip_smoke = false;
    let mut make_zip = true;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => {
                let raw = args.next().ok_or("--out 需要输出目录路径。")?;
                out_dir = Some(PathBuf::from(raw));
            }
            "--version" => {
                version = args.next().ok_or("--version 需要版本号。")?;
            }
            "--profile" => {
                profile = args.next().ok_or("--profile 需要 release 或 dist。")?;
                if !matches!(profile.as_str(), "release" | "dist") {
                    return Err(format!(
                        "不支持的 --profile：{profile}（仅允许 release 或 dist）"
                    ));
                }
            }
            "--require-update-config" => require_update_config = true,
            "--skip-web-build" => skip_web_build = true,
            "--skip-cargo-build" => skip_cargo_build = true,
            "--skip-smoke" => skip_smoke = true,
            "--no-zip" => make_zip = false,
            other => return Err(format!("未知的 dist 参数：{other}")),
        }
    }

    let out_dir =
        out_dir.ok_or("必须通过 --out <dir> 指定隔离输出目录（例如 E:\\tmp\\<日期>\\dist）。")?;
    if require_update_config && skip_cargo_build {
        return Err("--require-update-config 不能与 --skip-cargo-build 同时使用，必须使用本次环境变量重新编译全部二进制。".into());
    }
    cst_updater::manifest::version(&version)?;
    Ok(DistArgs {
        out_dir,
        version,
        profile,
        require_update_config,
        skip_web_build,
        skip_cargo_build,
        skip_smoke,
        make_zip,
    })
}

fn repo_root() -> Result<PathBuf, String> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "无法定位仓库根目录。".to_owned())
}

fn validate_dist_update_build_flags(
    has_update_config: bool,
    skip_cargo_build: bool,
) -> Result<(), String> {
    if has_update_config && skip_cargo_build {
        return Err(
            "已配置在线更新环境变量（CST_UPDATE_MANIFEST_URL / CST_UPDATE_PUBLIC_KEY），不能与 --skip-cargo-build 同时使用，必须重新编译全部二进制以绑定内嵌公钥。"
                .into(),
        );
    }
    Ok(())
}

fn run_dist(raw_args: impl Iterator<Item = String>) -> Result<(), String> {
    let args = parse_dist_args(raw_args)?;
    let update_cfg = release::resolve_update_build_config(args.require_update_config)?;
    validate_dist_update_build_flags(update_cfg.is_some(), args.skip_cargo_build)?;
    let attestation_key_file = if let Some(cfg) = &update_cfg
        && args.make_zip
    {
        Some(release::resolve_build_attestation_key_file(
            &cfg.public_key_fingerprint,
        )?)
    } else {
        None
    };
    let root = repo_root()?;
    let target_subdir = if args.profile == "dist" {
        "dist"
    } else {
        "release"
    };

    // 1. Optional Web frontend build
    if !args.skip_web_build {
        println!("[xtask] Building Web frontend...");
        let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
        let status = Command::new(npm)
            .args(["run", "build", "--prefix", "src/web/frontend"])
            .current_dir(&root)
            .status()
            .map_err(|e| format!("无法执行 npm run build：{e}"))?;
        if !status.success() {
            return Err(format!("Web 前端构建失败：{status}"));
        }
    }

    // 2. Optional Cargo build (with embed-web on cst-server; static CRT on Windows dist)
    if !args.skip_cargo_build {
        println!(
            "[xtask] Building Rust binaries (profile={}, updateEnabled={})...",
            args.profile,
            update_cfg.is_some()
        );
        let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let dist_rustflags = if cfg!(windows) && args.profile == "dist" {
            let mut flags = env::var("RUSTFLAGS").unwrap_or_default();
            if !flags.contains("crt-static") {
                if !flags.is_empty() {
                    flags.push(' ');
                }
                flags.push_str("-C target-feature=+crt-static");
            }
            Some(flags)
        } else {
            None
        };
        let mut cmd1 = Command::new(&cargo);
        cmd1.args([
            "build",
            "--profile",
            &args.profile,
            "--locked",
            "-p",
            "cst-desktop",
            "-p",
            "cst-updater",
        ])
        .env("CST_BUILD_VERSION", &args.version)
        .current_dir(&root);
        if let Some(cfg) = &update_cfg {
            cmd1.env(release::ENV_MANIFEST_URL, &cfg.manifest_url)
                .env(release::ENV_PUBLIC_KEY, &cfg.public_key_b64);
        }
        if let Some(flags) = &dist_rustflags {
            cmd1.env("RUSTFLAGS", flags);
        }
        let st1 = cmd1
            .status()
            .map_err(|e| format!("无法执行 cargo build：{e}"))?;
        if !st1.success() {
            return Err(format!("cst-desktop / cst-updater 编译失败：{st1}"));
        }

        let mut cmd2 = Command::new(&cargo);
        cmd2.args([
            "build",
            "--profile",
            &args.profile,
            "--locked",
            "-p",
            "cst-server",
            "--features",
            "embed-web",
        ])
        .env("CST_BUILD_VERSION", &args.version)
        .current_dir(&root);
        if let Some(cfg) = &update_cfg {
            cmd2.env(release::ENV_MANIFEST_URL, &cfg.manifest_url)
                .env(release::ENV_PUBLIC_KEY, &cfg.public_key_b64);
        }
        if let Some(flags) = &dist_rustflags {
            cmd2.env("RUSTFLAGS", flags);
        }
        let st2 = cmd2
            .status()
            .map_err(|e| format!("无法执行 cargo build (cst-server)：{e}"))?;
        if !st2.success() {
            return Err(format!("cst-server (embed-web) 编译失败：{st2}"));
        }
    }

    // 3. Assemble clean release tree under `<out>/cst-pilot/`
    let kit_dir = args.out_dir.join("cst-pilot");
    if kit_dir.exists() {
        fs::remove_dir_all(&kit_dir).map_err(|e| format!("无法清理旧输出目录：{e}"))?;
    }
    fs::create_dir_all(kit_dir.join("app")).map_err(|e| format!("无法创建发行目录：{e}"))?;

    let bin_dir = cargo_target_dir(&root)?.join(target_subdir);
    copy_required_file(
        &bin_dir.join("cst-pilot.exe"),
        &kit_dir.join("cst-pilot.exe"),
    )?;
    copy_required_file(
        &bin_dir.join("cst-server.exe"),
        &kit_dir.join(SERVER_EXE_PATH),
    )?;
    copy_required_file(
        &bin_dir.join("cst-updater.exe"),
        &kit_dir.join(UPDATER_EXE_PATH),
    )?;
    if let Some(cfg) = &update_cfg {
        release::verify_binaries_key_fingerprint(&kit_dir, &cfg.public_key_fingerprint)?;
    }

    for doc in [
        "README.md",
        "LICENSE",
        "THIRD-PARTY-NOTICES.md",
        "CHANGELOG.md",
    ] {
        let src = root.join(doc);
        if src.is_file() {
            copy_required_file(&src, &kit_dir.join(doc))?;
        }
    }

    fs::write(
        kit_dir.join("VERSION"),
        format!("cst-pilot {}\n", args.version),
    )
    .map_err(|e| format!("无法写入 VERSION：{e}"))?;

    let git_commit_full = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .to_ascii_lowercase()
        })
        .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()));
    let git_commit = git_commit_full
        .as_deref()
        .map(|s| s[..7].to_owned())
        .unwrap_or_else(|| "unknown".to_owned());
    let built_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut build_info = json!({
        "version": args.version,
        "gitCommit": git_commit,
        "builtAt": built_at,
        "profile": args.profile,
        "platform": "windows-x86_64",
        "embedWeb": true,
        "updateEnabled": update_cfg.is_some(),
    });
    if let Some(cfg) = &update_cfg
        && let Some(obj) = build_info.as_object_mut()
    {
        obj.insert(
            "updatePublicKeyFingerprint".to_owned(),
            json!(cfg.public_key_fingerprint),
        );
    }
    let mut build_info_bytes =
        serde_json::to_vec_pretty(&build_info).map_err(|_| "BUILD-INFO 序列化失败。")?;
    build_info_bytes.push(b'\n');
    fs::write(kit_dir.join(BUILD_INFO_PATH), build_info_bytes)
        .map_err(|e| format!("无法写入 BUILD-INFO.json：{e}"))?;

    // Copy prompts, skills, telemetry assets, defaults, licenses
    copy_required_file(
        &root.join("app/prompts/APPEND_SYSTEM.md"),
        &kit_dir.join("app/prompts/APPEND_SYSTEM.md"),
    )?;
    for skill in REQUIRED_SKILL_NAMES {
        copy_required_file(
            &root.join(format!("app/skills/{skill}/SKILL.md")),
            &kit_dir.join(format!("app/skills/{skill}/SKILL.md")),
        )?;
    }
    for rel in [
        "app/telemetry/timserver_1.crt",
        "app/telemetry/currency.json",
    ] {
        copy_required_file(&root.join(rel), &kit_dir.join(rel))?;
    }
    for rel in DEFAULT_CONFIG_FILES {
        copy_required_file(&root.join(rel), &kit_dir.join(rel))?;
    }
    copy_dir_filtered(
        &root.join("app/licenses"),
        &kit_dir.join("app/licenses"),
        |_| true,
    )?;
    // Ensure required Rust third-party license text (`ntfs-ng`) is always present in dist tree.
    copy_required_file(
        &root.join("app/licenses/ntfs-ng-LICENSE.txt"),
        &kit_dir.join("app/licenses/ntfs-ng-LICENSE.txt"),
    )?;

    // Copy shared fonts (prefer repo `app/fonts`, fallback `assets/fonts`)
    let fonts_src = if root.join("app/fonts").is_dir() {
        root.join("app/fonts")
    } else {
        root.join("assets/fonts")
    };
    for rel in REQUIRED_FONT_FILES {
        let name = rel.strip_prefix("app/fonts/").expect("font prefix");
        copy_required_file(&fonts_src.join(name), &kit_dir.join(rel))?;
    }

    // Copy external diagnostic tool (`lhm`) if present
    let lhm_src = if root.join("app/tools/lhm").is_dir() {
        root.join("app/tools/lhm")
    } else {
        root.join("lhm")
    };
    if lhm_src.is_dir() {
        copy_dir_filtered(&lhm_src, &kit_dir.join("app/tools/lhm"), |rel| {
            let lower = rel.to_lowercase();
            !lower
                .split('/')
                .any(|part| matches!(part, "obj" | "test" | "tests" | "src"))
        })?;
    }

    // 4. Redline & metadata scan using shared `cst-updater` validator
    println!("[xtask] Validating clean release tree...");
    let sums = cst_updater::verify::scan_clean_dist_tree(&kit_dir, &args.version)?;
    let sums_text = format_sums(&sums);
    fs::write(kit_dir.join(SHA256SUMS_PATH), sums_text.as_bytes())
        .map_err(|e| format!("无法写入 {SHA256SUMS_PATH}：{e}"))?;

    let mut all_paths: Vec<String> = sums.keys().cloned().collect();
    all_paths.push(SHA256SUMS_PATH.to_owned());
    let total_bytes = release::write_zip_entries_unpacked_size(&kit_dir, &all_paths)?;
    println!(
        "[xtask] Assembled {} files ({:.2} MiB, unpackedSize={} bytes incl. SHA256SUMS) under {}",
        all_paths.len(),
        total_bytes as f64 / (1024.0 * 1024.0),
        total_bytes,
        kit_dir.display()
    );

    // 5. Smoke the final package (or a temporary copy with --no-zip).
    // 6. Package ZIP archive
    if args.make_zip {
        let zip_name = format!("cst-pilot-{}-windows-x64.zip", args.version);
        let zip_path = args.out_dir.join(&zip_name);
        println!("[xtask] Packaging {}...", zip_path.display());
        write_release_zip(&kit_dir, &zip_path, &sums)?;
        if !args.skip_smoke {
            let temp = SmokeDir::new()?;
            let archive = File::open(&zip_path).map_err(|e| format!("无法打开发行 ZIP：{e}"))?;
            zip::ZipArchive::new(archive)
                .and_then(|mut archive| archive.extract(&temp.0))
                .map_err(|e| format!("解压发行 ZIP 失败：{e}"))?;
            smoke_release(
                &temp.0.join("cst-pilot"),
                &args.version,
                update_cfg
                    .as_ref()
                    .map(|cfg| cfg.public_key_fingerprint.as_str()),
            )?;
        }
        let zip_bytes = fs::read(&zip_path).map_err(|e| format!("读取发行 ZIP 失败：{e}"))?;
        let zip_hash = format!("{:x}", Sha256::digest(&zip_bytes));
        println!(
            "[xtask] ZIP ready: {} ({:.2} MiB, sha256={})",
            zip_path.display(),
            zip_bytes.len() as f64 / (1024.0 * 1024.0),
            zip_hash
        );
        if let (Some(cfg), Some(att_key_file)) = (&update_cfg, &attestation_key_file) {
            let full_commit = git_commit_full
                .clone()
                .ok_or("无法获取完整 40 位 Git HEAD 提交哈希以签发构建证明。")?;
            let attestation_path = args.out_dir.join(format!(
                "cst-pilot-{}-windows-x64.attestation.json",
                args.version
            ));
            let sums_hash = format!("{:x}", Sha256::digest(sums_text.as_bytes()));
            let updater_hash = sums
                .get(UPDATER_EXE_PATH)
                .cloned()
                .ok_or_else(|| format!("SHA256SUMS 缺少 {UPDATER_EXE_PATH}。"))?;
            let server_hash = sums
                .get(SERVER_EXE_PATH)
                .cloned()
                .ok_or_else(|| format!("SHA256SUMS 缺少 {SERVER_EXE_PATH}。"))?;
            let attestation = release::BuildAttestation {
                schema_version: 1,
                version: args.version.clone(),
                platform: "windows-x86_64".to_owned(),
                built_at: built_at.clone(),
                git_commit: full_commit,
                update_enabled: true,
                update_public_key_fingerprint: cfg.public_key_fingerprint.clone(),
                verified_key_probe: true,
                archive_filename: zip_name,
                archive_size: zip_bytes.len() as u64,
                archive_sha256: zip_hash,
                unpacked_size: total_bytes,
                sha256_sums_sha256: sums_hash,
                updater_exe_sha256: updater_hash,
                server_exe_sha256: server_hash,
            };
            let att_pub_fp = release::write_signed_build_attestation(
                &attestation_path,
                &attestation,
                att_key_file,
                &cfg.public_key_fingerprint,
            )?;
            println!(
                "[xtask] Signed build attestation ready: {} (issuerFingerprint={att_pub_fp})",
                attestation_path.display()
            );
        }
    } else if !args.skip_smoke {
        let temp = SmokeDir::new()?;
        copy_dir_filtered(&kit_dir, &temp.0, |_| true)?;
        smoke_release(
            &temp.0,
            &args.version,
            update_cfg
                .as_ref()
                .map(|cfg| cfg.public_key_fingerprint.as_str()),
        )?;
    }

    Ok(())
}

fn cargo_target_dir(root: &Path) -> Result<PathBuf, String> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args(["metadata", "--no-deps", "--format-version", "1", "--locked"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("无法执行 cargo metadata：{e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata 失败：{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("无法解析 Cargo 元数据：{e}"))?;
    metadata["target_directory"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| "Cargo 元数据缺少 target_directory。".into())
}

struct SmokeDir(PathBuf);

impl SmokeDir {
    fn new() -> Result<Self, String> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = env::temp_dir().join(format!("cst-dist-smoke-{}-{stamp}", std::process::id()));
        fs::create_dir(&path).map_err(|e| format!("无法创建冒烟目录：{e}"))?;
        Ok(Self(path))
    }
}

impl Drop for SmokeDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn isolated_command(root: &Path, exe: &str) -> Command {
    let mut cmd = Command::new(root.join(exe));
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
    cmd.current_dir(root)
        .args(["--kit-root"])
        .arg(root)
        .env("CST_PILOT_ROOT", root)
        .env("CST_OFFLINE", "1")
        .env("CST_ALLOW_UNELEVATED", "1");
    cmd
}

fn smoke_release(
    root: &Path,
    version: &str,
    expected_key_fingerprint: Option<&str>,
) -> Result<(), String> {
    cst_updater::verify::verify_clean_dist_tree(root, version)?;
    if let Some(expected_fp) = expected_key_fingerprint {
        release::verify_binaries_key_fingerprint(root, expected_fp)?;
    }
    let expected = format!("cst-pilot {version}");
    for exe in [SERVER_EXE_PATH, "cst-pilot.exe"] {
        let output = isolated_command(root, exe)
            .arg("--health-check")
            .output()
            .map_err(|e| format!("无法启动健康检查 {exe}：{e}"))?;
        if !output.status.success()
            || !String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.trim() == expected)
        {
            return Err(format!(
                "{exe} 健康检查失败：{}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    let config = root.join("data/config");
    if config.exists() {
        return Err("健康检查后出现用户配置，无法验证干净首启。".into());
    }
    let tmp = root.join(".state/tmp");
    fs::create_dir_all(&tmp).map_err(|e| format!("无法创建冒烟临时目录：{e}"))?;
    let mut child = isolated_command(root, SERVER_EXE_PATH)
        .args(["--port", "0", "--standalone-web"])
        .env("TEMP", &tmp)
        .env("TMP", &tmp)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| format!("无法启动首启冒烟服务：{e}"))?;
    let result: Result<(), String> = (|| {
        let deadline = Instant::now() + Duration::from_secs(20);
        let discovery = root.join(".state/server.json");
        loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                return Err(format!("首启冒烟服务提前退出：{status}"));
            }
            if discovery.is_file() {
                break;
            }
            if Instant::now() >= deadline {
                return Err("首启冒烟等待服务就绪超时。".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        for path in DEFAULT_CONFIG_FILES {
            let name = Path::new(path)
                .file_name()
                .ok_or_else(|| format!("默认配置路径无效：{path}"))?;
            let template =
                fs::read(root.join(path)).map_err(|e| format!("无法读取默认配置 {path}：{e}"))?;
            let initialized = fs::read(config.join(name))
                .map_err(|e| format!("首启未从模板 {path} 生成配置：{e}"))?;
            if template != initialized {
                return Err(format!("首启配置与默认模板不符：{path}"));
            }
        }
        Ok(())
    })();
    let _ = child.kill();
    let _ = child.wait();
    result?;
    println!("[xtask] ZIP/tree verification, health checks and first-start defaults OK");
    Ok(())
}

fn format_sums(sums: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    for (path, hash) in sums {
        out.push_str(hash);
        out.push_str("  ");
        out.push_str(path);
        out.push('\n');
    }
    out
}

fn copy_required_file(src: &Path, dst: &Path) -> Result<(), String> {
    if !src.is_file() {
        return Err(format!("缺少发行必需文件：{}", src.display()));
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("无法创建目标目录 {}：{e}", parent.display()))?;
    }
    fs::copy(src, dst)
        .map_err(|e| format!("复制文件失败 {} -> {}：{e}", src.display(), dst.display()))?;
    Ok(())
}

fn copy_dir_filtered<F>(src_root: &Path, dst_root: &Path, allow: F) -> Result<(), String>
where
    F: Fn(&str) -> bool,
{
    fn walk<F: Fn(&str) -> bool>(
        base: &Path,
        dir: &Path,
        dst_root: &Path,
        allow: &F,
    ) -> Result<(), String> {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .map_err(|e| format!("读取目录失败 {}：{e}", dir.display()))?
            .filter_map(Result::ok)
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let rel = path
                .strip_prefix(base)
                .map_err(|_| "目录相对路径计算失败。")?
                .to_string_lossy()
                .replace('\\', "/");
            if !allow(&rel) {
                continue;
            }
            let target = dst_root.join(&rel);
            let meta = fs::symlink_metadata(&path)
                .map_err(|e| format!("读取元数据失败 {}：{e}", path.display()))?;
            if cst_os::is_reparse_or_symlink(&meta) {
                return Err(format!("源目录禁止包含符号链接：{}", path.display()));
            }
            if meta.is_dir() {
                walk(base, &path, dst_root, allow)?;
            } else if meta.is_file() {
                copy_required_file(&path, &target)?;
            }
        }
        Ok(())
    }
    walk(src_root, src_root, dst_root, &allow)
}

fn write_release_zip(
    kit_dir: &Path,
    zip_path: &Path,
    sums: &BTreeMap<String, String>,
) -> Result<(), String> {
    let file = File::create(zip_path).map_err(|e| format!("无法创建 ZIP：{e}"))?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    let mut all_paths: Vec<String> = sums.keys().cloned().collect();
    all_paths.push(SHA256SUMS_PATH.to_owned());
    all_paths.sort();

    for rel in all_paths {
        let zip_entry = format!("cst-pilot/{rel}");
        zip.start_file(&zip_entry, options)
            .map_err(|e| format!("写入 ZIP 条目失败 {zip_entry}：{e}"))?;
        let mut src =
            File::open(kit_dir.join(&rel)).map_err(|e| format!("打开文件失败 {rel}：{e}"))?;
        io::copy(&mut src, &mut zip).map_err(|e| format!("压缩文件失败 {rel}：{e}"))?;
    }
    zip.finish()
        .map_err(|e| format!("完成 ZIP 归档失败：{e}"))?
        .flush()
        .map_err(|e| format!("刷新 ZIP 失败：{e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dist_rejects_skip_cargo_build_whenever_update_config_is_present() {
        assert!(validate_dist_update_build_flags(true, true).is_err());
        assert!(validate_dist_update_build_flags(true, false).is_ok());
        assert!(validate_dist_update_build_flags(false, true).is_ok());
        assert!(validate_dist_update_build_flags(false, false).is_ok());
    }
}

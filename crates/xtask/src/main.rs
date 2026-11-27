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

const ENV_STRICT_KEYLESS_BUILD: &str = "CST_STRICT_KEYLESS_BUILD";

fn main() -> ExitCode {
    if let Err(err) = scrub_or_reject_build_secrets_at_entry() {
        eprintln!("[xtask] ERROR: {err}");
        return ExitCode::FAILURE;
    }
    if let Err(err) = run() {
        eprintln!("[xtask] ERROR: {err}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Runs at single-threaded `fn main()` entry before any worker threads or child processes exist.
/// When running `dist` or `verify-tree`:
/// - under `--strict-keyless-env` or `CST_STRICT_KEYLESS_BUILD=1`, fails closed if release secrets
///   (`CST_RELEASE_PRIVATE_KEY_FILE` / `CST_RELEASE_ADMIN_TOKEN`) are present in the environment;
/// - otherwise purges both variables from `xtask`'s own process environment immediately so
///   maintainer workstations with user-scoped release variables never leak them to build tools.
fn scrub_or_reject_build_secrets_at_entry() -> Result<(), String> {
    let raw_args: Vec<String> = env::args().skip(1).collect();
    let Some(subcmd) = raw_args.first().map(String::as_str) else {
        return Ok(());
    };
    if !matches!(subcmd, "dist" | "verify-tree") {
        return Ok(());
    }
    let strict_flag = raw_args.iter().any(|a| a == "--strict-keyless-env");
    let strict_env = env::var(ENV_STRICT_KEYLESS_BUILD)
        .ok()
        .as_deref()
        .is_some_and(cst_os::is_truthy_env_value);
    let has_key = env::var_os(release::ENV_PRIVATE_KEY_FILE).is_some();
    let has_tok = env::var_os(release::ENV_ADMIN_TOKEN).is_some();
    if strict_flag || strict_env {
        return check_build_env_secret_vars(has_key, has_tok);
    }
    if has_key || has_tok {
        // SAFETY: Called strictly at single-threaded `fn main()` entry before spawning threads.
        unsafe {
            env::remove_var(release::ENV_PRIVATE_KEY_FILE);
            env::remove_var(release::ENV_ADMIN_TOKEN);
        }
        eprintln!(
            "[xtask] Scrubbed {} / {} from {subcmd} process environment.",
            release::ENV_PRIVATE_KEY_FILE,
            release::ENV_ADMIN_TOKEN
        );
    }
    Ok(())
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let Some(cmd) = args.next() else {
        print_usage();
        return Err(
            "缺少子命令（支持 dist / verify-tree / keygen / sign-release / publish-release / release）。"
                .into(),
        );
    };
    match cmd.as_str() {
        "dist" => run_dist(args),
        "verify-tree" => run_verify_tree(args),
        "keygen" => release::run_keygen(args),
        "sign-release" => release::run_sign_release(args),
        "publish-release" => release::run_publish_release(args),
        "release" => release::run_release(args),
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

struct VerifyTreeArgs {
    dir: PathBuf,
    version: Option<String>,
    strict_keyless_env: bool,
}

fn parse_verify_tree_args(args: impl Iterator<Item = String>) -> Result<VerifyTreeArgs, String> {
    let mut strict_keyless_env = false;
    let mut positionals = Vec::new();
    for arg in args {
        if arg == "--strict-keyless-env" {
            strict_keyless_env = true;
        } else if arg.starts_with('-') {
            return Err(format!("未知的 verify-tree 参数：{arg}"));
        } else {
            positionals.push(arg);
        }
    }
    if positionals.is_empty() || positionals.len() > 2 {
        return Err(
            "verify-tree 用法：xtask verify-tree [--strict-keyless-env] <dir> [version]".into(),
        );
    }
    let dir = PathBuf::from(positionals.remove(0));
    let version = positionals.pop();
    if let Some(ref v) = version {
        cst_updater::manifest::version(v)?;
    }
    Ok(VerifyTreeArgs {
        dir,
        version,
        strict_keyless_env,
    })
}

fn run_verify_tree(raw_args: impl Iterator<Item = String>) -> Result<(), String> {
    let parsed = parse_verify_tree_args(raw_args)?;
    if parsed.strict_keyless_env {
        check_build_env_secret_vars(
            env::var_os(release::ENV_PRIVATE_KEY_FILE).is_some(),
            env::var_os(release::ENV_ADMIN_TOKEN).is_some(),
        )?;
    }
    let target_version = match parsed.version {
        Some(v) => v,
        None => cst_updater::layout::read_version(&parsed.dir)?,
    };
    let count = cst_updater::verify::verify_clean_dist_tree(&parsed.dir, &target_version)?;
    release::verify_canonical_release_defaults(&parsed.dir)?;
    println!("[xtask] verify-tree PASS: {count} files validated for {target_version}");
    Ok(())
}

fn print_usage() {
    eprintln!(
        "Usage (pre-compile xtask in a keyless shell first via `cargo build --release -p xtask --locked`):\n  xtask dist --out <dir> [--version <vX.Y.Z>] [--profile release|dist] [--channel stable|beta|disabled] [--require-update-config] [--strict-keyless-env] [--skip-web-build] [--skip-cargo-build] [--skip-smoke] [--no-zip]\n  xtask verify-tree [--strict-keyless-env] <dir> [version]\n  xtask keygen --out <key-file>\n  xtask sign-release [--dist-dir <dir>] --zip <path> --attestation <path> --expected-attestation-sha256 <64-hex> --expected-commit <40-hex> --min-upgradable <vX.Y.Z> --out <manifest.signed.json> [--version <vX.Y.Z>] [--base-url <url>] [--notes-file <md> | --notes <text>] [--private-key-file <path>]\n  xtask publish-release --manifest <path> --zip <path> --admin-url <url> [--channel stable|beta] [--no-activate] [--public-base-url <url>] [--public-manifest-url <url>] [--public-archive-url <url>] [--public-key <base64>]\n  xtask release --package <zip-or-dir> --min-upgradable <vX.Y.Z> [--channel stable|beta] [--attestation <path> --expected-attestation-sha256 <64-hex> --expected-commit <40-hex> | --expected-archive-sha256 <64-hex>] [--version <vX.Y.Z>] [--notes-file <md> | --notes <text>] [--private-key-file <path>] [--out-manifest <path>] [--ssh-host <alias> [--ssh-remote-port <port>] | --admin-url <url>] [--public-base-url <url>] [--no-activate] [--dry-run]"
    );
}

struct DistArgs {
    out_dir: PathBuf,
    version: String,
    profile: String,
    channel: Option<release::DistChannel>,
    require_update_config: bool,
    strict_keyless_env: bool,
    skip_web_build: bool,
    skip_cargo_build: bool,
    skip_smoke: bool,
    make_zip: bool,
}

fn parse_dist_args(mut args: impl Iterator<Item = String>) -> Result<DistArgs, String> {
    let mut out_dir: Option<PathBuf> = None;
    let mut version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let mut profile = "release".to_owned();
    let mut channel: Option<release::DistChannel> = None;
    let mut require_update_config = false;
    let mut strict_keyless_env = false;
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
            "--channel" => {
                let raw = args
                    .next()
                    .ok_or("--channel 需要 stable、beta 或 disabled。")?;
                channel = Some(release::DistChannel::parse(&raw)?);
            }
            "--require-update-config" => require_update_config = true,
            "--strict-keyless-env" => strict_keyless_env = true,
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
        channel,
        require_update_config,
        strict_keyless_env,
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
    skip_web_build: bool,
    make_zip: bool,
) -> Result<(), String> {
    if has_update_config && (skip_cargo_build || skip_web_build || !make_zip) {
        return Err(
            "启用在线更新打包时禁止使用 --skip-cargo-build、--skip-web-build 或 --no-zip（仅本地调试可指定 --channel disabled）；正式构建必须从干净源码全量编译并产出 ZIP 与构建证明。"
                .into(),
        );
    }
    Ok(())
}

fn check_build_env_secret_vars(
    has_private_key_var: bool,
    has_admin_token_var: bool,
) -> Result<(), String> {
    if has_private_key_var || has_admin_token_var {
        return Err(format!(
            "严格无密钥构建模式禁止环境中存在 {} 或 {}，请将在线构建与离线签名/发布环境严格隔离。",
            release::ENV_PRIVATE_KEY_FILE,
            release::ENV_ADMIN_TOKEN
        ));
    }
    Ok(())
}

pub(crate) fn strip_release_secrets(cmd: &mut Command) -> &mut Command {
    cmd.env_remove(release::ENV_PRIVATE_KEY_FILE)
        .env_remove(release::ENV_ADMIN_TOKEN)
}

fn verify_clean_git_head(root: &Path) -> Result<String, String> {
    let mut status_cmd = Command::new("git");
    strip_release_secrets(&mut status_cmd);
    let status = status_cmd
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("无法执行 git status 检查干净工作树：{e}"))?;
    if !status.status.success() {
        return Err("git status 执行失败，无法验证发布构建源码来源。".into());
    }
    if !String::from_utf8_lossy(&status.stdout).trim().is_empty() {
        return Err(
            "启用在线更新打包要求 Git 工作树完全干净（无未提交或未跟踪文件），拒绝打包。".into(),
        );
    }
    let mut head_cmd = Command::new("git");
    strip_release_secrets(&mut head_cmd);
    let head = head_cmd
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("无法执行 git rev-parse HEAD：{e}"))?;
    if !head.status.success() {
        return Err("无法读取完整 40 位 Git HEAD 提交哈希。".into());
    }
    let sha = String::from_utf8_lossy(&head.stdout)
        .trim()
        .to_ascii_lowercase();
    if sha.len() != 40
        || !sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("Git HEAD 不是有效的 40 位小写十六进制提交哈希。".into());
    }
    Ok(sha)
}

fn run_dist(raw_args: impl Iterator<Item = String>) -> Result<(), String> {
    let args = parse_dist_args(raw_args)?;
    if args.strict_keyless_env {
        check_build_env_secret_vars(
            env::var_os(release::ENV_PRIVATE_KEY_FILE).is_some(),
            env::var_os(release::ENV_ADMIN_TOKEN).is_some(),
        )?;
    }
    let update_cfg =
        release::resolve_update_build_config(args.channel, args.require_update_config)?;
    validate_dist_update_build_flags(
        update_cfg.is_some(),
        args.skip_cargo_build,
        args.skip_web_build,
        args.make_zip,
    )?;
    let root = repo_root()?;
    // Online-update builds never hold any private key; instead they lock to an unmodified Git
    // commit before and after compilation so the resulting build attestation binds clean source.
    let pre_build_clean_head = if update_cfg.is_some() {
        Some(verify_clean_git_head(&root)?)
    } else {
        None
    };
    let target_subdir = if args.profile == "dist" {
        "dist"
    } else {
        "release"
    };

    // 1. Optional Web frontend build
    if !args.skip_web_build {
        println!("[xtask] Building Web frontend...");
        let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
        let mut npm_cmd = Command::new(npm);
        strip_release_secrets(&mut npm_cmd);
        let status = npm_cmd
            .args(["run", "build", "--prefix", "src/web/frontend"])
            .current_dir(&root)
            .status()
            .map_err(|e| format!("无法执行 npm run build：{e}"))?;
        if !status.success() {
            return Err(format!("Web 前端构建失败：{status}"));
        }
    }

    // 2. Optional Cargo build (with embed-web on cst-server; static CRT on Windows dist)
    let built_executables = if !args.skip_cargo_build {
        release::verify_embedded_web_build_dir(&root.join("src/web/frontend/build/client"), &root)?;
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
        strip_release_secrets(&mut cmd1);
        cmd1.args([
            "build",
            "--profile",
            &args.profile,
            "--locked",
            "--message-format=json-render-diagnostics",
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
        } else {
            cmd1.env(release::ENV_MANIFEST_URL, "disabled")
                .env(release::ENV_PUBLIC_KEY, "disabled");
        }
        if let Some(flags) = &dist_rustflags {
            cmd1.env("RUSTFLAGS", flags);
        }
        let mut exes = run_cargo_and_collect_executables(&mut cmd1, "cst-desktop / cst-updater")?;

        let mut cmd2 = Command::new(&cargo);
        strip_release_secrets(&mut cmd2);
        cmd2.args([
            "build",
            "--profile",
            &args.profile,
            "--locked",
            "--message-format=json-render-diagnostics",
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
        } else {
            cmd2.env(release::ENV_MANIFEST_URL, "disabled")
                .env(release::ENV_PUBLIC_KEY, "disabled");
        }
        if let Some(flags) = &dist_rustflags {
            cmd2.env("RUSTFLAGS", flags);
        }
        exes.extend(run_cargo_and_collect_executables(
            &mut cmd2,
            "cst-server (embed-web)",
        )?);
        Some(exes)
    } else {
        None
    };

    // 3. Assemble clean release tree under `<out>/cst-pilot/`
    let kit_dir = args.out_dir.join("cst-pilot");
    if kit_dir.exists() {
        fs::remove_dir_all(&kit_dir).map_err(|e| format!("无法清理旧输出目录：{e}"))?;
    }
    fs::create_dir_all(kit_dir.join("app")).map_err(|e| format!("无法创建发行目录：{e}"))?;

    if let Some(exes) = &built_executables {
        let pilot_src = exes
            .get("cst-pilot")
            .ok_or("Cargo 编译输出未报告 cst-pilot 可执行产物路径。")?;
        let server_src = exes
            .get("cst-server")
            .ok_or("Cargo 编译输出未报告 cst-server 可执行产物路径。")?;
        let updater_src = exes
            .get("cst-updater")
            .ok_or("Cargo 编译输出未报告 cst-updater 可执行产物路径。")?;
        copy_required_file(pilot_src, &kit_dir.join("cst-pilot.exe"))?;
        copy_required_file(server_src, &kit_dir.join(SERVER_EXE_PATH))?;
        copy_required_file(updater_src, &kit_dir.join(UPDATER_EXE_PATH))?;
    } else {
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
    }

    for rel in ["cst-pilot.exe", SERVER_EXE_PATH, UPDATER_EXE_PATH] {
        let bin_bytes =
            fs::read(kit_dir.join(rel)).map_err(|e| format!("无法读取发行二进制 {rel}：{e}"))?;
        release::verify_pe64_windows_gui_binary(&bin_bytes, rel)?;
    }

    if let Some(cfg) = &update_cfg {
        release::verify_static_binary_update_root(
            &kit_dir,
            &cfg.manifest_url,
            &cfg.public_key_b64,
        )?;
        release::verify_binaries_key_fingerprint(&kit_dir, &cfg.public_key_fingerprint)?;
    } else {
        release::verify_binaries_update_disabled(&kit_dir)?;
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

    let mut git_rev_cmd = Command::new("git");
    strip_release_secrets(&mut git_rev_cmd);
    let git_commit_full = git_rev_cmd
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

    // 4. Redline & metadata scan using shared `cst-updater` validator + canonical defaults check
    println!("[xtask] Validating clean release tree...");
    let sums = cst_updater::verify::scan_clean_dist_tree(&kit_dir, &args.version)?;
    release::verify_canonical_release_defaults(&kit_dir)?;
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
            let smoke_kit = temp.0.join("cst-pilot");
            fs::create_dir_all(&smoke_kit)
                .map_err(|e| format!("无法创建冒烟解压目录 {}：{e}", smoke_kit.display()))?;
            cst_updater::verify::extract_clean_dist_archive(
                &zip_path,
                &smoke_kit,
                cst_updater::manifest::MAX_UNPACKED_BYTES,
            )?;
            smoke_release(
                &smoke_kit,
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
        if let Some(cfg) = &update_cfg {
            let expected_head = pre_build_clean_head
                .as_deref()
                .ok_or("缺少构建前干净工作树 HEAD 记录。")?;
            let post_build_head = verify_clean_git_head(&root)?;
            if post_build_head != expected_head {
                return Err(format!(
                    "构建过程中 Git HEAD 发生变化（{expected_head} -> {post_build_head}），拒绝产出构建证明。"
                ));
            }
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
                git_commit: post_build_head.clone(),
                update_enabled: true,
                manifest_url: cfg.manifest_url.clone(),
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
            let att_sha256 = release::write_build_attestation(&attestation_path, &attestation)?;
            println!(
                "[xtask] Build attestation ready: {} (sha256={att_sha256}, commit={post_build_head})",
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

fn run_cargo_and_collect_executables(
    cmd: &mut Command,
    label: &str,
) -> Result<BTreeMap<String, PathBuf>, String> {
    use std::io::{BufRead, BufReader};
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("无法启动 cargo build ({label})：{e}"))?;
    let mut exes = BTreeMap::new();
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines() {
            let line = line.map_err(|e| format!("读取 cargo 输出失败 ({label})：{e}"))?;
            let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if msg.get("reason").and_then(serde_json::Value::as_str) != Some("compiler-artifact") {
                continue;
            }
            let is_bin = msg
                .get("target")
                .and_then(|t| t.get("kind"))
                .and_then(serde_json::Value::as_array)
                .is_some_and(|kinds| kinds.iter().any(|k| k.as_str() == Some("bin")));
            let Some(name) = msg
                .get("target")
                .and_then(|t| t.get("name"))
                .and_then(serde_json::Value::as_str)
            else {
                continue;
            };
            let Some(exe) = msg.get("executable").and_then(serde_json::Value::as_str) else {
                continue;
            };
            if is_bin {
                exes.insert(name.to_owned(), PathBuf::from(exe));
            }
        }
    }
    let status = child
        .wait()
        .map_err(|e| format!("等待 cargo build 结束失败 ({label})：{e}"))?;
    if !status.success() {
        return Err(format!("{label} 编译失败：{status}"));
    }
    Ok(exes)
}

fn cargo_target_dir(root: &Path) -> Result<PathBuf, String> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    strip_release_secrets(&mut cmd);
    let output = cmd
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
    release::verify_canonical_release_defaults(root)?;
    if let Some(expected_fp) = expected_key_fingerprint {
        release::verify_binaries_key_fingerprint(root, expected_fp)?;
    } else {
        release::verify_binaries_update_disabled(root)?;
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

pub(crate) fn copy_dir_filtered<F>(src_root: &Path, dst_root: &Path, allow: F) -> Result<(), String>
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

pub(crate) fn write_release_zip(
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
    use std::ffi::OsStr;

    #[test]
    fn dist_rejects_skip_build_or_no_zip_whenever_update_config_is_present() {
        assert!(validate_dist_update_build_flags(true, true, false, true).is_err());
        assert!(validate_dist_update_build_flags(true, false, true, true).is_err());
        assert!(validate_dist_update_build_flags(true, false, false, false).is_err());
        assert!(validate_dist_update_build_flags(true, false, false, true).is_ok());
        assert!(validate_dist_update_build_flags(false, true, true, false).is_ok());
        assert!(validate_dist_update_build_flags(false, false, false, true).is_ok());
    }

    #[test]
    fn dist_strict_keyless_mode_rejects_secrets_and_strip_helper_removes_them() {
        assert!(check_build_env_secret_vars(true, false).is_err());
        assert!(check_build_env_secret_vars(false, true).is_err());
        assert!(check_build_env_secret_vars(true, true).is_err());
        assert!(check_build_env_secret_vars(false, false).is_ok());

        let mut cmd = Command::new("cargo");
        strip_release_secrets(&mut cmd);
        let envs: Vec<_> = cmd.get_envs().collect();
        assert!(envs.contains(&(OsStr::new(release::ENV_PRIVATE_KEY_FILE), None)));
        assert!(envs.contains(&(OsStr::new(release::ENV_ADMIN_TOKEN), None)));
    }

    #[test]
    fn parse_dist_args_supports_channels_and_strict_keyless_flag() {
        let parsed = parse_dist_args(
            [
                "--out",
                "E:\\tmp\\dist",
                "--channel",
                "beta",
                "--strict-keyless-env",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .expect("valid dist args");
        assert_eq!(parsed.channel, Some(release::DistChannel::Beta));
        assert!(parsed.strict_keyless_env);

        let disabled = parse_dist_args(
            [
                "--out",
                "E:\\tmp\\dist",
                "--channel",
                "disabled",
                "--skip-web-build",
                "--no-zip",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .expect("valid disabled dist args");
        assert_eq!(disabled.channel, Some(release::DistChannel::Disabled));
        assert!(!disabled.make_zip);
    }

    #[test]
    fn parse_verify_tree_args_accepts_strict_keyless_flag_in_any_position() {
        let parsed = parse_verify_tree_args(
            ["--strict-keyless-env", "E:\\tmp\\kit", "v0.9.8"]
                .into_iter()
                .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(parsed.dir, PathBuf::from("E:\\tmp\\kit"));
        assert_eq!(parsed.version.as_deref(), Some("v0.9.8"));
        assert!(parsed.strict_keyless_env);

        assert!(parse_verify_tree_args(["--unknown"].into_iter().map(str::to_owned)).is_err());
    }
}

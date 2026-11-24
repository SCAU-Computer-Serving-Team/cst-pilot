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
        return Err("缺少子命令（支持 dist / verify-tree）。".into());
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
        "Usage:\n  cargo run -p xtask -- dist --out <dir> [--version <vX.Y.Z>] [--profile release|dist] [--skip-web-build] [--skip-cargo-build] [--skip-smoke] [--no-zip]\n  cargo run -p xtask -- verify-tree <dir> [version]"
    );
}

struct DistArgs {
    out_dir: PathBuf,
    version: String,
    profile: String,
    skip_web_build: bool,
    skip_cargo_build: bool,
    skip_smoke: bool,
    make_zip: bool,
}

fn parse_dist_args(mut args: impl Iterator<Item = String>) -> Result<DistArgs, String> {
    let mut out_dir: Option<PathBuf> = None;
    let mut version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let mut profile = "release".to_owned();
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
            "--skip-web-build" => skip_web_build = true,
            "--skip-cargo-build" => skip_cargo_build = true,
            "--skip-smoke" => skip_smoke = true,
            "--no-zip" => make_zip = false,
            other => return Err(format!("未知的 dist 参数：{other}")),
        }
    }

    let out_dir =
        out_dir.ok_or("必须通过 --out <dir> 指定隔离输出目录（例如 E:\\tmp\\<日期>\\dist）。")?;
    cst_updater::manifest::version(&version)?;
    Ok(DistArgs {
        out_dir,
        version,
        profile,
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

fn run_dist(raw_args: impl Iterator<Item = String>) -> Result<(), String> {
    let args = parse_dist_args(raw_args)?;
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
            "[xtask] Building Rust binaries (profile={})...",
            args.profile
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

    let git_commit = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(&root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned());
    let build_info = json!({
        "version": args.version,
        "gitCommit": git_commit,
        "builtAt": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        "profile": args.profile,
        "platform": "windows-x86_64",
        "embedWeb": true,
    });
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

    let mut total_bytes = 0u64;
    for rel in sums.keys() {
        total_bytes += fs::metadata(kit_dir.join(rel))
            .map(|m| m.len())
            .unwrap_or(0);
    }
    println!(
        "[xtask] Assembled {} files ({:.2} MiB) under {}",
        sums.len() + 1,
        total_bytes as f64 / (1024.0 * 1024.0),
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
            smoke_release(&temp.0.join("cst-pilot"), &args.version)?;
        }
        let zip_bytes = fs::read(&zip_path).map_err(|e| format!("读取发行 ZIP 失败：{e}"))?;
        let zip_hash = format!("{:x}", Sha256::digest(&zip_bytes));
        println!(
            "[xtask] ZIP ready: {} ({:.2} MiB, sha256={})",
            zip_path.display(),
            zip_bytes.len() as f64 / (1024.0 * 1024.0),
            zip_hash
        );
    } else if !args.skip_smoke {
        let temp = SmokeDir::new()?;
        copy_dir_filtered(&kit_dir, &temp.0, |_| true)?;
        smoke_release(&temp.0, &args.version)?;
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

fn smoke_release(root: &Path, version: &str) -> Result<(), String> {
    cst_updater::verify::verify_clean_dist_tree(root, version)?;
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

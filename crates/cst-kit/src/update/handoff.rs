use std::path::Path;

use cst_os::{KitLayout, Storage, parse_local_json};
pub use cst_updater::handoff::{Handoff, LaunchReceipt, run_request};

use crate::update::layout;

/// The business executable calls this before starting services when given `--health-check`.
/// It performs only local checks, without creating credentials, telemetry, or contacting a provider.
pub fn health_check(root: &Path) -> Result<(), String> {
    layout::read_version(root)?;
    let kit = crate::Kit::new(root)?;
    kit.check_resources()?;
    let layout_paths = kit.layout();
    let storage = Storage::new(layout_paths.root())?;
    let fonts_rel = layout_paths.fonts_relative_dir();
    let _fonts = storage.pin_directory(fonts_rel, false)?;
    for name in [
        "SourceHanSansCN-VF.otf",
        "LICENSE.txt",
        "JetBrainsMono-Regular.ttf",
        "JetBrainsMono-Medium.ttf",
        "JetBrainsMono-OFL.txt",
    ] {
        if storage
            .read_limited(&storage.root.join(fonts_rel).join(name), 32 * 1024 * 1024)?
            .is_none()
        {
            return Err("健康检查缺少必需随包字体或许可文件。".into());
        }
    }
    for (name, magic) in [
        ("SourceHanSansCN-VF.otf", b"OTTO"),
        ("JetBrainsMono-Regular.ttf", &[0, 1, 0, 0]),
        ("JetBrainsMono-Medium.ttf", &[0, 1, 0, 0]),
    ] {
        let bytes = storage
            .read_limited(&storage.root.join(fonts_rel).join(name), 32 * 1024 * 1024)?
            .ok_or("健康检查缺少随包字体。")?;
        layout::validate_sfnt_font_bytes(&bytes, magic)?;
    }
    let config_dir = layout_paths.config_dir();
    if config_dir.exists() {
        let (data_storage, data_rel) = open_data_storage_readonly(layout_paths)?;
        let cfg_rel = data_rel.join("config");
        let _cfg = data_storage.pin_directory(&cfg_rel, false)?;
        for name in [
            "auth.json",
            "models.json",
            "settings.json",
            "device.json",
            "desktop.json",
            "web.json",
        ] {
            if let Some(bytes) = data_storage.read_limited(
                &data_storage.root.join(&cfg_rel).join(name),
                8 * 1024 * 1024,
            )? && !parse_local_json(&format!("data/config/{name}"), &bytes)?.is_object()
            {
                return Err("健康检查发现配置格式无效，原文件已保留。".into());
            }
        }
    }
    Ok(())
}

fn open_data_storage_readonly(layout: &KitLayout) -> Result<(Storage, std::path::PathBuf), String> {
    if let Ok(rel) = layout.data_dir().strip_prefix(layout.root()) {
        Ok((Storage::new(layout.root())?, rel.to_path_buf()))
    } else {
        Ok((Storage::new(layout.data_dir())?, std::path::PathBuf::new()))
    }
}

/// Normalizes optional leading `'v'` on release version tags (`"v0.9.5"` == `"0.9.5"`).
pub fn normalize_version_tag(raw: &str) -> &str {
    let trimmed = raw.trim();
    trimmed.strip_prefix('v').unwrap_or(trimmed)
}

/// Runs `health_check(root)` and verifies an optional binary-compiled build version against `<kit_root>/VERSION`.
/// Returns the verified `VERSION` string (`<ver>`) for `--health-check` stdout.
pub fn run_cli_health_check_with_version(
    root: &Path,
    compiled_build_version: Option<&str>,
) -> Result<String, String> {
    health_check(root)?;
    let disk_version = layout::read_version(root)?;
    if let Some(build_ver) = compiled_build_version
        .map(str::trim)
        .filter(|s| !s.is_empty())
        && normalize_version_tag(&disk_version) != normalize_version_tag(build_ver)
    {
        return Err("健康检查发现程序内置版本与工具包 VERSION 不一致。".into());
    }
    Ok(disk_version)
}

/// Runs `health_check(root)` and returns the verified `VERSION` string (`<ver>`) for `--health-check` stdout.
pub fn run_cli_health_check(root: &Path) -> Result<String, String> {
    run_cli_health_check_with_version(root, option_env!("CST_BUILD_VERSION"))
}

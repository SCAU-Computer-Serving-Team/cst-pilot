use std::path::Path;

use cst_os::{Storage, parse_local_json};
pub use cst_updater::handoff::{Handoff, LaunchReceipt, run_request};

use crate::update::layout;

/// The business executable calls this before starting services when given `--health-check`.
/// It performs only local checks, without creating credentials, telemetry, or contacting a provider.
pub fn health_check(root: &Path) -> Result<(), String> {
    layout::read_version(root)?;
    crate::Kit::new(root)?.check_resources()?;
    let storage = Storage::new(root)?;
    let _fonts = storage.pin_directory(Path::new("assets/fonts"), false)?;
    for rel in layout::REQUIRED_FONT_FILES {
        if storage
            .read_limited(&storage.root.join(rel), 32 * 1024 * 1024)?
            .is_none()
        {
            return Err("健康检查缺少必需随包字体或许可文件。".into());
        }
    }
    for (rel, magic) in [
        ("assets/fonts/SourceHanSansCN-VF.otf", b"OTTO"),
        ("assets/fonts/JetBrainsMono-Regular.ttf", &[0, 1, 0, 0]),
        ("assets/fonts/JetBrainsMono-Medium.ttf", &[0, 1, 0, 0]),
    ] {
        let bytes = storage
            .read_limited(&storage.root.join(rel), 32 * 1024 * 1024)?
            .ok_or("健康检查缺少随包字体。")?;
        layout::validate_sfnt_font_bytes(&bytes, magic)?;
    }
    let _home = storage.pin_directory(Path::new("agent/home"), false)?;
    for name in [
        "auth.json",
        "models.json",
        "settings.json",
        "device.json",
        "desktop.json",
    ] {
        if let Some(bytes) =
            storage.read_limited(&storage.root.join("agent/home").join(name), 8 * 1024 * 1024)?
            && !parse_local_json(&format!("agent/home/{name}"), &bytes)?.is_object()
        {
            return Err("健康检查发现配置格式无效，原文件已保留。".into());
        }
    }
    Ok(())
}

/// Runs `health_check(root)` and returns the verified `VERSION` string (`<ver>`) for `--health-check` stdout.
pub fn run_cli_health_check(root: &Path) -> Result<String, String> {
    health_check(root)?;
    layout::read_version(root)
}

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
    let _home = storage.pin_directory(Path::new("agent/home"), false)?;
    for name in ["auth.json", "models.json", "settings.json", "device.json"] {
        if let Some(bytes) =
            storage.read_limited(&storage.root.join("agent/home").join(name), 8 * 1024 * 1024)?
            && !parse_local_json(&format!("agent/home/{name}"), &bytes)?.is_object()
        {
            return Err("健康检查发现配置格式无效，原文件已保留。".into());
        }
    }
    Ok(())
}

use super::manifest::version;
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    Program,
    Merge,
}
pub const MERGE_FILES: &[&str] = &[
    "agent/home/models-store.json",
    "agent/home/telemetry.json",
    "agent/home/web-search.json",
    "agent/home/settings.json",
];

/// ZIP and SHA256SUMS use one portable, Windows-safe spelling for each path.
pub fn safe_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.encode_utf16().count() > 240
        || path.contains('\\')
        || path
            .chars()
            .any(|c| c.is_control() || ":*?\"<>|".contains(c))
    {
        return Err("升级包路径无效。".into());
    }
    for part in path.split('/') {
        let stem = part
            .split('.')
            .next()
            .unwrap_or("")
            .trim_end()
            .to_uppercase();
        let device = matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|n| {
                matches!(
                    n,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        });
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.trim() != part
            || part.ends_with('.')
            || device
        {
            return Err("升级包路径含保留名称或越界组件。".into());
        }
    }
    Ok(())
}
pub fn classify(path: &str) -> Result<Area, String> {
    safe_path(path)?;
    let lower = path.to_lowercase();
    let parts: Vec<_> = lower.split('/').collect();
    if parts.iter().any(|p| {
        matches!(
            *p,
            ".state"
                | ".git"
                | ".cache"
                | "node-compile-cache"
                | "target"
                | "src"
                | "test"
                | "tests"
                | "obj"
                | ".env"
                | "auth.json"
                | "models.json"
                | "mcp-auth.json"
                | "device.json"
                | "web-settings.json"
                | "identity.json"
        ) || p.starts_with(".env.")
            || *p == "agents.md"
    }) || lower.ends_with(".otf")
        || lower.starts_with("wiztree/tmp/")
        || matches!(
            lower.as_str(),
            "wiztree/wiztree3.ini" | "wiztree/wiztree3.ini.bad"
        )
    {
        return Err("升级包包含运行状态或开发文件。".into());
    }
    if MERGE_FILES.contains(&path) {
        return Ok(Area::Merge);
    }
    let file = matches!(
        path,
        "cst-pilot.exe"
            | "cst-pilot-desktop.exe"
            | "cst-updater.exe"
            | "pi.cmd"
            | "pi-web.cmd"
            | "launcher.ps1"
            | "VERSION"
            | "BUILD-INFO.json"
            | "SHA256SUMS"
            | "README.md"
            | "CHANGELOG.md"
            | "LICENSE"
            | "THIRD-PARTY-NOTICES.md"
            | "biome.json"
            | "agent/home/APPEND_SYSTEM.md"
            | "agent/home/open-tui.json"
    );
    let directory = [
        "assets/",
        "doc/",
        "licenses/",
        "pwsh/",
        "wiztree/",
        "lhm/",
        "agent/.runtime/",
        "agent/node_modules/",
        "agent/home/extensions/",
        "agent/home/skills/",
        "agent/home/packages/",
        "agent/home/bin/",
    ]
    .iter()
    .any(|p| path.starts_with(p));
    if file || directory || (!path.contains('/') && lower.ends_with(".dll")) {
        Ok(Area::Program)
    } else {
        Err("升级包含程序白名单外的路径，用户数据不可随包替换。".into())
    }
}

/// Arrays are user choices and remain intact. Models-store merges by stable model id.
pub fn merge_defaults(
    path: &str,
    current: Option<&[u8]>,
    defaults: &[u8],
) -> Result<Vec<u8>, String> {
    if !MERGE_FILES.contains(&path)
        || defaults.len() > 8 * 1024 * 1024
        || current.is_some_and(|v| v.len() > 8 * 1024 * 1024)
    {
        return Err("预置配置路径或大小无效。".into());
    }
    let parse = |bytes: &[u8]| -> Result<Value, String> {
        let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
        let value = super::json::parse(bytes)?;
        if !value.is_object() {
            return Err("升级配置必须为 JSON 对象，原文件已保留。".into());
        }
        Ok(value)
    };
    let next = parse(defaults)?;
    let Some(current) = current else {
        return serde_json::to_vec_pretty(&next).map_err(|_| "配置无法编码。".into());
    };
    let mut old = super::json::parse_local(path, current)?;
    if !old.is_object() {
        return Err("升级配置必须为 JSON 对象，原文件已保留。".into());
    }
    if path.ends_with("models-store.json") {
        merge_catalog(&mut old, &next);
    } else {
        merge_object(&mut old, &next);
    }
    let mut bytes = serde_json::to_vec_pretty(&old).map_err(|_| "配置无法编码。")?;
    bytes.push(b'\n');
    Ok(bytes)
}
fn merge_object(old: &mut Value, next: &Value) {
    if let (Some(old), Some(next)) = (old.as_object_mut(), next.as_object()) {
        for (key, value) in next {
            if let Some(existing) = old.get_mut(key) {
                merge_object(existing, value);
            } else {
                old.insert(key.clone(), value.clone());
            }
        }
    }
}
fn merge_catalog(old: &mut Value, next: &Value) {
    let (Some(old), Some(next)) = (old.as_object_mut(), next.as_object()) else {
        return;
    };
    for (provider, value) in next {
        let Some(existing) = old.get_mut(provider) else {
            old.insert(provider.clone(), value.clone());
            continue;
        };
        if let (Some(models), Some(extra)) = (
            existing.get_mut("models").and_then(Value::as_array_mut),
            value.get("models").and_then(Value::as_array),
        ) {
            for model in extra {
                if let Some(id) = model.get("id").and_then(Value::as_str)
                    && !models
                        .iter()
                        .any(|m| m.get("id").and_then(Value::as_str) == Some(id))
                {
                    models.push(model.clone());
                }
            }
        }
        // Preserve the existing etag and timestamp, including dynamically fetched entries.
        merge_object(existing, value);
    }
}

pub fn read_version(root: &Path) -> Result<String, String> {
    let storage = crate::runbook::storage::Storage::new(root)?;
    let _pin = storage.pin_directory(Path::new(""), false)?;
    let bytes = storage
        .read_limited(&storage.root.join("VERSION"), 128)?
        .ok_or("缺少工具包 VERSION。")?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "VERSION 编码无效。")?;
    let value = text
        .lines()
        .find_map(|line| line.strip_prefix("cst-pilot "))
        .ok_or("VERSION 缺少工具包版本。")?;
    version(value)?;
    Ok(value.into())
}
pub(crate) fn validate_metadata(
    files: &BTreeMap<String, String>,
    root: &Path,
    target: &str,
) -> Result<(), String> {
    for path in [
        "cst-pilot.exe",
        "cst-updater.exe",
        "VERSION",
        "BUILD-INFO.json",
        "LICENSE",
        "THIRD-PARTY-NOTICES.md",
        "agent/home/APPEND_SYSTEM.md",
    ] {
        if !files.contains_key(path) {
            return Err("升级包缺少必需的程序、版本或许可文件。".into());
        }
    }
    for skill in crate::SKILL_NAMES {
        if !files.contains_key(&format!("agent/home/skills/{skill}/SKILL.md")) {
            return Err("升级包缺少维护技能。".into());
        }
    }
    if !files.contains_key("agent/home/extensions/telemetry/timserver_1.crt") {
        return Err("升级包缺少遥测证书。".into());
    }
    if !files.keys().any(|p| p.starts_with("licenses/")) {
        return Err("升级包缺少第三方许可目录。".into());
    }
    if read_version(root)? != target {
        return Err("升级包 VERSION 与签名清单不一致。".into());
    }
    let storage = crate::runbook::storage::Storage::new(root)?;
    let _pin = storage.pin_directory(Path::new(""), false)?;
    let bytes = storage
        .read_limited(&root.join("BUILD-INFO.json"), 64 * 1024)?
        .ok_or("升级包缺少构建信息。")?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "升级构建信息无效。")?;
    if value.get("version").and_then(Value::as_str) != Some(target) {
        return Err("升级构建信息的版本不一致。".into());
    }
    Ok(())
}

pub fn available_space(root: &Path) -> Result<u64, String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::{Win32::Storage::FileSystem::GetDiskFreeSpaceExW, core::PCWSTR};
        let path: Vec<_> = root.as_os_str().encode_wide().chain([0]).collect();
        let mut available = 0;
        unsafe { GetDiskFreeSpaceExW(PCWSTR(path.as_ptr()), Some(&mut available), None, None) }
            .map_err(|_| "无法读取升级目录所在卷的剩余空间。")?;
        Ok(available)
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        Err("软件内升级仅支持 Windows。".into())
    }
}

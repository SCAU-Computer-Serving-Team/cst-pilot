use std::collections::BTreeMap;
use std::path::Path;

use cst_os::Storage;
use serde_json::Value;

use crate::manifest::version;

pub const SHA256SUMS_PATH: &str = "app/SHA256SUMS";
pub const BUILD_INFO_PATH: &str = "app/BUILD-INFO.json";
pub const SERVER_EXE_PATH: &str = "app/cst-server.exe";
pub const UPDATER_EXE_PATH: &str = "app/cst-updater.exe";

pub use cst_os::REQUIRED_SKILL_NAMES;

pub const REQUIRED_FONT_FILES: &[&str] = &[
    "app/fonts/SourceHanSansCN-VF.otf",
    "app/fonts/LICENSE.txt",
    "app/fonts/JetBrainsMono-Regular.ttf",
    "app/fonts/JetBrainsMono-Medium.ttf",
    "app/fonts/JetBrainsMono-OFL.txt",
];

pub const DEFAULT_CONFIG_FILES: &[&str] = &[
    "app/defaults/models-store.json",
    "app/defaults/telemetry.json",
    "app/defaults/web-search.json",
    "app/defaults/settings.json",
];

pub const MERGE_FILES: &[&str] = &[
    "data/config/models-store.json",
    "data/config/telemetry.json",
    "data/config/web-search.json",
    "data/config/settings.json",
];

pub const DEFAULT_MERGE_TARGETS: &[(&str, &str)] = &[
    (
        "app/defaults/models-store.json",
        "data/config/models-store.json",
    ),
    ("app/defaults/telemetry.json", "data/config/telemetry.json"),
    (
        "app/defaults/web-search.json",
        "data/config/web-search.json",
    ),
    ("app/defaults/settings.json", "data/config/settings.json"),
];

pub fn validate_sfnt_font_bytes(bytes: &[u8], expected_magic: &[u8; 4]) -> Result<(), String> {
    if bytes.len() < 28 || &bytes[..4] != expected_magic {
        return Err("随包字体格式无效或头部损坏。".into());
    }
    let num_tables = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
    let dir_end = 12usize
        .checked_add(num_tables.saturating_mul(16))
        .ok_or("随包字体表目录溢出。")?;
    if num_tables == 0 || bytes.len() < dir_end {
        return Err("随包字体表目录损坏。".into());
    }
    let mut has_cmap = false;
    let mut has_name = false;
    for i in 0..num_tables {
        let entry = 12 + i * 16;
        let tag = &bytes[entry..entry + 4];
        let offset = u32::from_be_bytes(bytes[entry + 8..entry + 12].try_into().unwrap()) as usize;
        let len = u32::from_be_bytes(bytes[entry + 12..entry + 16].try_into().unwrap()) as usize;
        if offset.checked_add(len).is_none_or(|end| end > bytes.len()) {
            return Err("随包字体表数据越界。".into());
        }
        if tag == b"cmap" {
            has_cmap = true;
        } else if tag == b"name" {
            has_name = true;
        }
    }
    if !has_cmap || !has_name {
        return Err("随包字体缺少必需字符映射或名称表。".into());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    Program,
    Merge,
}

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
                | "sessions"
                | "plans"
                | "inbox"
                | "exports"
                | "artifacts"
                | "outbox"
                | "session-exports"
                | "command-plans"
                | "fff"
                | "npm"
                | ".env"
                | "auth.json"
                | "models.json"
                | "mcp-auth.json"
                | "device.json"
                | "web.json"
                | "web-settings.json"
                | "desktop.json"
                | "desktop-settings.json"
                | "tui.json"
                | "identity.json"
        ) || p.starts_with(".env.")
            || *p == "agents.md"
    }) || (lower.ends_with(".otf") && path != "app/fonts/SourceHanSansCN-VF.otf")
    {
        return Err("升级包包含运行状态或开发文件。".into());
    }
    if MERGE_FILES.contains(&path) {
        return Ok(Area::Merge);
    }
    let root_file = matches!(
        path,
        "cst-pilot.exe"
            | "VERSION"
            | "README.md"
            | "CHANGELOG.md"
            | "LICENSE"
            | "THIRD-PARTY-NOTICES.md"
    );
    let app_allowed = if let Some(rest) = path.strip_prefix("app/") {
        !rest.contains('/')
            || matches!(
                path,
                "app/telemetry/timserver_1.crt" | "app/telemetry/currency.json"
            )
            || matches!(
                rest.split('/').next().unwrap_or(""),
                "defaults" | "docs" | "fonts" | "licenses" | "prompts" | "skills" | "tools"
            )
    } else {
        false
    };
    if root_file || app_allowed || (!path.contains('/') && lower.ends_with(".dll")) {
        Ok(Area::Program)
    } else {
        Err("升级包含程序白名单外的路径，用户数据不可随包替换。".into())
    }
}

pub(crate) fn reject_retired_release_path(path: &str) -> Result<(), String> {
    let path = path.to_ascii_lowercase();
    if ["wiztree", "app/tools/wiztree"].iter().any(|prefix| {
        path == *prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('/'))
    }) {
        return Err("发行包包含已移除的组件路径。".into());
    }
    Ok(())
}

pub(crate) fn classify_release(path: &str) -> Result<Area, String> {
    let area = classify(path)?;
    reject_retired_release_path(path)?;
    Ok(area)
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
        let value = crate::json::parse(bytes)?;
        if !value.is_object() {
            return Err("升级配置必须为 JSON 对象，原文件已保留。".into());
        }
        Ok(value)
    };
    let next = parse(defaults)?;
    let Some(current) = current else {
        let mut bytes = serde_json::to_vec_pretty(&next).map_err(|_| "配置无法编码。")?;
        bytes.push(b'\n');
        return Ok(bytes);
    };
    let mut old = crate::json::parse_local(path, current)?;
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
        merge_object(existing, value);
    }
}

pub fn read_version(root: &Path) -> Result<String, String> {
    let storage = Storage::new(root)?;
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

pub fn validate_metadata(
    files: &BTreeMap<String, String>,
    root: &Path,
    target: &str,
) -> Result<(), String> {
    for path in [
        "cst-pilot.exe",
        SERVER_EXE_PATH,
        UPDATER_EXE_PATH,
        "VERSION",
        BUILD_INFO_PATH,
        "LICENSE",
        "THIRD-PARTY-NOTICES.md",
        "app/prompts/APPEND_SYSTEM.md",
        "app/telemetry/timserver_1.crt",
        "app/telemetry/currency.json",
    ] {
        if !files.contains_key(path) {
            return Err("升级包缺少必需的程序、版本或许可文件。".into());
        }
    }
    for default_cfg in DEFAULT_CONFIG_FILES {
        if !files.contains_key(*default_cfg) {
            return Err("升级包缺少必需的默认配置模板。".into());
        }
    }
    for font in REQUIRED_FONT_FILES {
        if !files.contains_key(*font) {
            return Err("升级包缺少必需的随包字体或许可文件。".into());
        }
    }
    for skill in REQUIRED_SKILL_NAMES {
        if !files.contains_key(&format!("app/skills/{skill}/SKILL.md")) {
            return Err("升级包缺少维护技能。".into());
        }
    }
    if !files.keys().any(|p| p.starts_with("app/licenses/")) {
        return Err("升级包缺少第三方许可目录。".into());
    }
    if read_version(root)? != target {
        return Err("升级包 VERSION 与签名清单不一致。".into());
    }
    let storage = Storage::new(root)?;
    let _pin = storage.pin_directory(Path::new("app"), false)?;
    let bytes = storage
        .read_limited(&root.join(BUILD_INFO_PATH), 64 * 1024)?
        .ok_or("升级包缺少构建信息。")?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "升级构建信息无效。")?;
    if value.get("version").and_then(Value::as_str) != Some(target) {
        return Err("升级构建信息的版本不一致。".into());
    }
    let _fonts_pin = storage.pin_directory(Path::new("app/fonts"), false)?;
    for (rel, magic) in [
        ("app/fonts/SourceHanSansCN-VF.otf", b"OTTO"),
        ("app/fonts/JetBrainsMono-Regular.ttf", &[0, 1, 0, 0]),
        ("app/fonts/JetBrainsMono-Medium.ttf", &[0, 1, 0, 0]),
    ] {
        let font_bytes = storage
            .read_limited(&root.join(rel), 32 * 1024 * 1024)?
            .ok_or("升级包缺少随包字体。")?;
        validate_sfnt_font_bytes(&font_bytes, magic)?;
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

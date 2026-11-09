use super::{Com, Control, NativeError, StartupSnapshot, native_error, stopped, wide};
use crate::diagnostics::startup::approval_disabled;
use serde_json::json;
use std::collections::HashMap;
use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_PATH_NOT_FOUND, WIN32_ERROR,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, REG_BINARY, REG_EXPAND_SZ, REG_SZ,
    RegCloseKey, RegEnumValueW, RegOpenKeyExW,
};
use windows::Win32::UI::Shell::{
    FOLDERID_CommonStartup, FOLDERID_Startup, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
};
use windows::core::{PCWSTR, PWSTR};
const BASE: &str = r"Software\Microsoft\Windows\CurrentVersion";
struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}
struct RegValue {
    name: String,
    kind: u32,
    bytes: Vec<u8>,
}
fn registry_error(path: &str, error: WIN32_ERROR) -> NativeError {
    NativeError {
        source: path.into(),
        code: Some(error.0),
        message: windows::core::Error::from_hresult(error.to_hresult()).message(),
    }
}
fn values(hive: HKEY, path: &str, control: &Control) -> Result<Vec<RegValue>, NativeError> {
    let mut key = HKEY::default();
    let path_wide = wide(path);
    let status = unsafe {
        RegOpenKeyExW(
            hive,
            PCWSTR(path_wide.as_ptr()),
            Some(0),
            KEY_READ,
            &mut key,
        )
    };
    if status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND {
        return Ok(Vec::new());
    }
    if status.is_err() {
        return Err(registry_error(path, status));
    }
    let key = Key(key);
    let mut out = Vec::new();
    let mut index = 0;
    let mut names = vec![0u16; 32768];
    let mut data = vec![0u8; 65536];
    loop {
        stopped("Registry", control)?;
        let mut name_length = names.len() as u32;
        let mut length = data.len() as u32;
        let mut kind = 0;
        let status = unsafe {
            RegEnumValueW(
                key.0,
                index,
                Some(PWSTR(names.as_mut_ptr())),
                &mut name_length,
                None,
                Some(&mut kind),
                Some(data.as_mut_ptr()),
                Some(&mut length),
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            break;
        }
        if status == ERROR_MORE_DATA && data.len() < 1024 * 1024 {
            data.resize((length as usize).max(data.len() * 2).min(1024 * 1024), 0);
            continue;
        }
        if status.is_err() {
            return Err(registry_error(path, status));
        }
        out.push(RegValue {
            name: String::from_utf16_lossy(&names[..name_length as usize]),
            kind,
            bytes: data[..length as usize].to_vec(),
        });
        index += 1;
    }
    Ok(out)
}
fn query(
    hive: HKEY,
    path: &str,
    control: &Control,
    errors: &mut Vec<NativeError>,
) -> Vec<RegValue> {
    match values(hive, path, control) {
        Ok(v) => v,
        Err(e) => {
            errors.push(e);
            Vec::new()
        }
    }
}
fn string(bytes: &[u8]) -> String {
    let mut chars: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    while chars.last() == Some(&0) {
        chars.pop();
    }
    String::from_utf16_lossy(&chars)
}

pub fn snapshot(control: &Control) -> Result<StartupSnapshot, NativeError> {
    let _com = Com::new()?;
    let mut out = StartupSnapshot::default();
    let mut approval: HashMap<(String, String, String), Vec<u8>> = HashMap::new();
    for (hive, label) in [(HKEY_CURRENT_USER, "HKCU"), (HKEY_LOCAL_MACHINE, "HKLM")] {
        for kind in ["Run", "Run32", "StartupFolder"] {
            let path = format!(r"{BASE}\Explorer\StartupApproved\{kind}");
            for value in query(hive, &path, control, &mut out.errors) {
                if value.kind == REG_BINARY.0 {
                    approval.insert(
                        (label.into(), kind.into(), value.name.to_lowercase()),
                        value.bytes,
                    );
                }
            }
        }
    }
    for (hive, label, path, once, kind) in [
        (
            HKEY_LOCAL_MACHINE,
            "HKLM",
            format!(r"{BASE}\Run"),
            false,
            "Run",
        ),
        (
            HKEY_LOCAL_MACHINE,
            "HKLM",
            format!(r"{BASE}\RunOnce"),
            true,
            "Run",
        ),
        (
            HKEY_LOCAL_MACHINE,
            "HKLM",
            r"Software\Wow6432Node\Microsoft\Windows\CurrentVersion\Run".into(),
            false,
            "Run32",
        ),
        (
            HKEY_CURRENT_USER,
            "HKCU",
            format!(r"{BASE}\Run"),
            false,
            "Run",
        ),
        (
            HKEY_CURRENT_USER,
            "HKCU",
            format!(r"{BASE}\RunOnce"),
            true,
            "Run",
        ),
    ] {
        for value in query(hive, &path, control, &mut out.errors) {
            if value.kind != REG_SZ.0 && value.kind != REG_EXPAND_SZ.0 {
                out.errors.push(NativeError {
                    source: format!(r"{label}\{path}"),
                    code: None,
                    message: "自启命令值类型不是字符串，已跳过。".into(),
                });
                continue;
            }
            let disabled = approval_disabled(
                once,
                approval
                    .get(&(label.into(), kind.into(), value.name.to_lowercase()))
                    .map(Vec::as_slice),
            );
            out.reg_items.push(json!({"source":format!(r"{label}\{path}"),"name":value.name,"command":string(&value.bytes),"disabled":disabled}));
        }
    }
    for (scope, hive, id) in [
        ("user", "HKCU", FOLDERID_Startup),
        ("allUsers", "HKLM", FOLDERID_CommonStartup),
    ] {
        stopped("StartupFolder", control)?;
        let pointer = match unsafe { SHGetKnownFolderPath(&id, KF_FLAG_DEFAULT, None) } {
            Ok(p) => p,
            Err(e) => {
                out.errors.push(native_error("SHGetKnownFolderPath", e));
                continue;
            }
        };
        let path = unsafe { pointer.to_string() };
        unsafe { CoTaskMemFree(Some(pointer.0.cast())) };
        let path = match path {
            Ok(p) => p,
            Err(e) => {
                out.errors.push(native_error("StartupFolder", e.into()));
                continue;
            }
        };
        let mut items = Vec::new();
        match std::fs::read_dir(&path) {
            Ok(entries) => {
                for entry in entries {
                    stopped("StartupFolder", control)?;
                    match entry.and_then(|entry| Ok((entry.file_name(), entry.file_type()?))) {
                        Ok((name, kind)) if kind.is_file() => {
                            let name = name.to_string_lossy();
                            let disabled = approval_disabled(
                                false,
                                approval
                                    .get(&(
                                        hive.into(),
                                        "StartupFolder".into(),
                                        name.to_lowercase(),
                                    ))
                                    .map(Vec::as_slice),
                            );
                            items.push(json!({"name":name,"disabled":disabled}));
                        }
                        Ok(_) => {}
                        Err(e) => out.errors.push(NativeError {
                            source: "StartupFolder".into(),
                            code: e.raw_os_error().map(|c| c as u32),
                            message: e.to_string(),
                        }),
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => out.errors.push(NativeError {
                source: "StartupFolder".into(),
                code: e.raw_os_error().map(|c| c as u32),
                message: e.to_string(),
            }),
        }
        items.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        out.folders
            .push(json!({"scope":scope,"path":path,"items":items}));
    }
    Ok(out)
}

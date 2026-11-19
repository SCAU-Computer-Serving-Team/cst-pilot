use serde::{Deserialize, Serialize};

use super::identity::Installation;

/// Probe values are transient. Raw MachineGuid is discarded before snapshot serialization.
#[derive(Default)]
pub struct SystemProbe {
    pub machine_guid: Option<String>,
    pub name: Option<String>,
    pub version: String,
    pub display_version: Option<String>,
    pub arch: Option<String>,
    pub is_pe: Option<bool>,
    pub admin: Option<bool>,
}
pub trait EnvironmentSource: Send + Sync {
    fn probe(&self) -> SystemProbe;
}
pub struct NativeEnvironment;
impl EnvironmentSource for NativeEnvironment {
    fn probe(&self) -> SystemProbe {
        native_probe()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub installation_id: Option<String>,
    pub environment_id: Option<String>,
    pub environment_source: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Os {
    pub platform: String,
    pub name: Option<String>,
    pub version: String,
    pub display_version: Option<String>,
    pub arch: Option<String>,
    #[serde(rename = "isPE")]
    pub is_pe: Option<bool>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Runtime {
    pub version: String,
    pub arch: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Environment {
    pub identity: Identity,
    pub os: Os,
    pub runtime: Runtime,
    pub admin: Option<bool>,
}
impl Environment {
    pub fn capture(installation: Option<&Installation>, source: &dyn EnvironmentSource) -> Self {
        let mut system = source.probe();
        let environment_id = if system.is_pe == Some(false) {
            installation
                .zip(system.machine_guid.take())
                .filter(|(_, guid)| !guid.trim().is_empty())
                .map(|(id, guid)| id.environment_id(&guid))
        } else {
            None
        };
        Self {
            identity: Identity {
                installation_id: installation.map(|id| id.installation_id.clone()),
                environment_source: if environment_id.is_some() {
                    "windows-machine-guid"
                } else {
                    "unavailable"
                }
                .into(),
                environment_id,
            },
            os: Os {
                platform: if cfg!(windows) {
                    "win32"
                } else {
                    std::env::consts::OS
                }
                .into(),
                name: system.name,
                version: system.version,
                display_version: system.display_version,
                arch: system.arch,
                is_pe: system.is_pe,
            },
            runtime: Runtime {
                version: env!("CARGO_PKG_VERSION").into(),
                arch: match std::env::consts::ARCH {
                    "x86_64" => "x64",
                    "aarch64" => "arm64",
                    "x86" => "ia32",
                    other => other,
                }
                .into(),
            },
            admin: system.admin,
        }
    }
}
#[cfg(not(windows))]
fn native_probe() -> SystemProbe {
    SystemProbe::default()
}
#[cfg(windows)]
fn native_probe() -> SystemProbe {
    use crate::diagnostics::win_native::{DiagnosticSource, WindowsSource};
    use windows::Win32::System::SystemInformation::{
        GetNativeSystemInfo, PROCESSOR_ARCHITECTURE_AMD64, PROCESSOR_ARCHITECTURE_ARM64,
        PROCESSOR_ARCHITECTURE_INTEL, SYSTEM_INFO,
    };
    let base = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let major = reg_number(base, "CurrentMajorVersionNumber");
    let minor = reg_number(base, "CurrentMinorVersionNumber");
    let build = reg_text(base, "CurrentBuildNumber");
    let mut name = reg_text(base, "ProductName");
    if build
        .as_deref()
        .and_then(|v| v.parse::<u32>().ok())
        .is_some_and(|build| build >= 22000)
    {
        name = name.map(|name| name.replacen("Windows 10", "Windows 11", 1));
    }
    let is_pe = registry_key_exists(r"SYSTEM\CurrentControlSet\Control\MiniNT");
    let mut info = SYSTEM_INFO::default();
    unsafe { GetNativeSystemInfo(&mut info) };
    let arch = match unsafe { info.Anonymous.Anonymous.wProcessorArchitecture } {
        PROCESSOR_ARCHITECTURE_AMD64 => Some("x64"),
        PROCESSOR_ARCHITECTURE_ARM64 => Some("arm64"),
        PROCESSOR_ARCHITECTURE_INTEL => Some("ia32"),
        _ => None,
    }
    .map(str::to_owned);
    SystemProbe {
        machine_guid: if is_pe == Some(false) {
            reg_text(r"SOFTWARE\Microsoft\Cryptography", "MachineGuid")
        } else {
            None
        },
        name,
        version: match (major, minor, build) {
            (Some(major), Some(minor), Some(build)) => format!("{major}.{minor}.{build}"),
            _ => String::new(),
        },
        display_version: reg_text(base, "DisplayVersion"),
        arch,
        is_pe,
        admin: WindowsSource.admin().ok(),
    }
}
#[cfg(windows)]
fn registry_key_exists(path: &str) -> Option<bool> {
    use windows::Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND},
        System::Registry::{
            HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY, RegCloseKey, RegOpenKeyExW,
        },
    };
    use windows::core::PCWSTR;
    let path: Vec<u16> = path.encode_utf16().chain([0]).collect();
    let mut key = HKEY::default();
    let result = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(path.as_ptr()),
            Some(0),
            KEY_READ | KEY_WOW64_64KEY,
            &mut key,
        )
    };
    if result.is_ok() {
        unsafe {
            let _ = RegCloseKey(key);
        };
        Some(true)
    } else if result == ERROR_FILE_NOT_FOUND || result == ERROR_PATH_NOT_FOUND {
        Some(false)
    } else {
        None
    }
}
#[cfg(windows)]
fn reg_value(
    path: &str,
    name: &str,
    kind: windows::Win32::System::Registry::REG_ROUTINE_FLAGS,
) -> Option<Vec<u8>> {
    use windows::Win32::System::Registry::{
        HKEY_LOCAL_MACHINE, RRF_SUBKEY_WOW6464KEY, RegGetValueW,
    };
    use windows::core::PCWSTR;
    let path: Vec<u16> = path.encode_utf16().chain([0]).collect();
    let name: Vec<u16> = name.encode_utf16().chain([0]).collect();
    let mut buffer = vec![0u8; 8192];
    let mut size = buffer.len() as u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(path.as_ptr()),
            PCWSTR(name.as_ptr()),
            kind | RRF_SUBKEY_WOW6464KEY,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if result.is_err() || size as usize > buffer.len() {
        return None;
    }
    buffer.truncate(size as usize);
    Some(buffer)
}
#[cfg(windows)]
fn reg_text(path: &str, name: &str) -> Option<String> {
    use windows::Win32::System::Registry::RRF_RT_REG_SZ;
    let bytes = reg_value(path, name, RRF_RT_REG_SZ)?;
    if bytes.len() % 2 != 0 {
        return None;
    }
    let wide: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .take_while(|c| *c != 0)
        .collect();
    String::from_utf16(&wide)
        .ok()
        .filter(|text| !text.trim().is_empty())
}
#[cfg(windows)]
fn reg_number(path: &str, name: &str) -> Option<u32> {
    use windows::Win32::System::Registry::RRF_RT_REG_DWORD;
    let bytes = reg_value(path, name, RRF_RT_REG_DWORD)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

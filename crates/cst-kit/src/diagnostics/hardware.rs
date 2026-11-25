use super::win_native::{Control, NativeError};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(windows)]
mod pdh;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CounterSet {
    Gpu,
    Sensor,
}
#[derive(Clone, Debug)]
pub struct CounterSample {
    pub counter: String,
    pub instance: String,
    pub value: f64,
}
#[derive(Clone, Default)]
pub struct CounterBatch {
    pub samples: Vec<CounterSample>,
    pub errors: Vec<NativeError>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LhmMode {
    Gpu,
    Sensor,
}
impl LhmMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Gpu => "gpu",
            Self::Sensor => "sensor",
        }
    }
}
pub trait HardwareSource: Send + Sync {
    fn counters(&self, set: CounterSet, control: &Control) -> Result<CounterBatch, String>;
    fn gpu_engine(&self, control: &Control) -> Result<CounterBatch, String> {
        self.counters(CounterSet::Gpu, control)
    }
    fn pawnio(&self) -> Result<bool, NativeError>;
    fn nvidia(&self, control: &Control) -> Result<Option<Value>, String>;
    fn lhm(&self, mode: LhmMode, control: &Control) -> Result<Value, String>;
}
pub struct NativeHardware {
    root: Option<PathBuf>,
}
impl NativeHardware {
    pub fn new(root: Option<PathBuf>) -> Self {
        Self { root }
    }
}
impl HardwareSource for NativeHardware {
    fn counters(&self, set: CounterSet, control: &Control) -> Result<CounterBatch, String> {
        #[cfg(windows)]
        {
            pdh::sample(set, false, control)
        }
        #[cfg(not(windows))]
        {
            let _ = (set, control);
            Err("PDH 仅支持 Windows。".into())
        }
    }
    #[cfg(windows)]
    fn gpu_engine(&self, control: &Control) -> Result<CounterBatch, String> {
        pdh::sample(CounterSet::Gpu, true, control)
    }
    fn pawnio(&self) -> Result<bool, NativeError> {
        #[cfg(windows)]
        {
            pdh::pawnio()
        }
        #[cfg(not(windows))]
        {
            Err(NativeError::unsupported("PawnIO"))
        }
    }
    fn nvidia(&self, control: &Control) -> Result<Option<Value>, String> {
        #[cfg(windows)]
        let exe = {
            use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
            let mut buffer = [0u16; 32768];
            let len = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
            if len == 0 || len >= buffer.len() {
                return Err("无法定位 Windows 系统目录。".into());
            }
            use std::os::windows::ffi::OsStringExt;
            let system = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..len]));
            find_nvidia_smi(&system, program_files)
        };
        #[cfg(not(windows))]
        let exe: Option<PathBuf> = None;
        let Some(exe) = exe else { return Ok(None) };
        let bounded = Control::new(
            control.cancel.child_token(),
            control.remaining().min(Duration::from_secs(8)),
        );
        let args = ["--query-gpu=uuid,pci.bus_id,name,temperature.gpu,power.draw,memory.used,memory.total,utilization.gpu,driver_version", "--format=csv,noheader,nounits"].map(Into::into);
        let output = super::native_job::run(&exe, &args, &bounded, 256 * 1024)?;
        parse_nvidia(&output).map(Some)
    }
    fn lhm(&self, mode: LhmMode, control: &Control) -> Result<Value, String> {
        let root = self.root.as_ref().ok_or("未配置 LHM 工具包目录。")?;
        let layout = cst_os::KitLayout::new(root)?;
        let lhm_dir = layout.lhm_dir();
        let exe = lhm_dir.join("host/cst-lhm-host.exe");
        let dll = lhm_dir.join("LibreHardwareMonitorLib.dll");
        if !exe.is_file() || !dll.is_file() {
            return Err("随包 LHM 辅助程序或 DLL 缺失。".into());
        }
        // Neither the model nor a working-directory search can choose the executable/library.
        ensure_component(layout.root(), &exe)?;
        ensure_component(layout.root(), &dll)?;
        let bounded = Control::new(
            control.cancel.child_token(),
            control.remaining().min(Duration::from_secs(20)),
        );
        let output = super::native_job::run(&exe, &[mode.name().into()], &bounded, 256 * 1024)?;
        let value: Value =
            serde_json::from_slice(&output).map_err(|_| "LHM 辅助程序返回的 JSON 无法解析。")?;
        if let Some(error) = value["error"].as_str() {
            return Err(format!("LHM：{error}"));
        }
        if !value["hardware"].is_array() || !value["sensors"].is_array() {
            return Err("LHM 辅助程序返回字段不完整。".into());
        }
        Ok(value)
    }
}
#[cfg(windows)]
fn program_files() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_ProgramFiles, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    let raw =
        unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramFiles, KF_FLAG_DEFAULT, None) }.ok()?;
    use std::os::windows::ffi::OsStringExt;
    let path = unsafe { std::ffi::OsString::from_wide(raw.as_wide()) };
    unsafe { CoTaskMemFree(Some(raw.0.cast())) };
    Some(PathBuf::from(path))
}

#[cfg(any(test, windows))]
fn find_nvidia_smi(
    system: &Path,
    program_files: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    let current = system.join("nvidia-smi.exe");
    if current.is_file() {
        return Some(current);
    }
    let root = program_files()?;
    let legacy = root
        .join("NVIDIA Corporation")
        .join("NVSMI")
        .join("nvidia-smi.exe");
    (root.is_absolute() && legacy.is_file() && ensure_component(&root, &legacy).is_ok())
        .then_some(legacy)
}

fn ensure_component(root: &Path, target: &Path) -> Result<(), String> {
    let relative = target
        .strip_prefix(root)
        .map_err(|_| "LHM 组件路径越界。")?;
    let mut path = root.to_path_buf();
    for part in relative.components() {
        path.push(part);
        let meta = std::fs::symlink_metadata(&path).map_err(|_| "LHM 组件无法访问。")?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Err("LHM 组件路径不允许重解析点。".into());
            }
        }
        if meta.is_symlink() {
            return Err("LHM 组件路径不允许符号链接。".into());
        }
    }
    Ok(())
}
pub fn parse_nvidia(output: &[u8]) -> Result<Value, String> {
    let text = std::str::from_utf8(output).map_err(|_| "nvidia-smi 输出编码无效。")?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .trim(csv::Trim::All)
        .from_reader(text.as_bytes());
    let mut rows = Vec::new();
    for row in reader.records() {
        let row = row.map_err(|_| "nvidia-smi 返回格式异常。")?;
        if row.len() != 9 || row[0].is_empty() || row[8].is_empty() || rows.len() >= 256 {
            return Err("nvidia-smi 返回格式异常或显卡数量超限。".into());
        }
        let n = |i: usize| row[i].parse::<f64>().ok().filter(|n| n.is_finite());
        rows.push(json!({"uuid":row[0],"pciBusId":row[1],"name":row[2],"tempC":n(3),"powerW":n(4),"vramUsedMB":n(5),"vramTotalMB":n(6),"utilPct":n(7),"driver":row[8]}));
    }
    if rows.is_empty() {
        return Err("nvidia-smi 未返回显卡。".into());
    }
    Ok(json!(rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvidia_lookup_prefers_system32_and_falls_back_to_known_program_files() {
        let dir = std::env::temp_dir().join(format!("cst-nvidia-{}", uuid::Uuid::new_v4()));
        let system = dir.join("System32");
        let program_files = dir.join("Program Files");
        let legacy = program_files
            .join("NVIDIA Corporation")
            .join("NVSMI")
            .join("nvidia-smi.exe");
        std::fs::create_dir_all(&system).unwrap();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, []).unwrap();
        assert_eq!(
            find_nvidia_smi(&system, || Some(program_files)),
            Some(legacy)
        );
        let current = system.join("nvidia-smi.exe");
        std::fs::write(&current, []).unwrap();
        assert_eq!(
            find_nvidia_smi(&system, || panic!("must not resolve Program Files")),
            Some(current)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}

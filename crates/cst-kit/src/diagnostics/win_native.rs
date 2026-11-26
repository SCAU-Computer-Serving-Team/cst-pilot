use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::WindowsSource;

pub(super) fn with_wmi_scope<T>(run: impl FnOnce() -> T) -> T {
    #[cfg(windows)]
    return windows::with_wmi_scope(run);
    #[cfg(not(windows))]
    run()
}
#[cfg(not(windows))]
pub struct WindowsSource;

pub const CIMV2: &str = r"ROOT\CIMV2";
pub const STORAGE: &str = r"ROOT\Microsoft\Windows\Storage";

#[derive(Clone)]
pub struct Control {
    pub cancel: CancellationToken,
    deadline: Instant,
}
impl Control {
    pub fn new(cancel: CancellationToken, timeout: Duration) -> Self {
        Self {
            cancel,
            deadline: Instant::now() + timeout,
        }
    }
    pub fn check(&self) -> Result<(), String> {
        if self.cancel.is_cancelled() {
            Err("诊断已取消。".into())
        } else if Instant::now() >= self.deadline {
            Err("诊断超时。".into())
        } else {
            Ok(())
        }
    }
    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
    pub fn sleep(&self, duration: Duration) -> Result<(), String> {
        let end = Instant::now() + duration;
        while Instant::now() < end {
            self.check()?;
            std::thread::sleep(
                end.saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(20)),
            );
        }
        self.check()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeError {
    pub source: String,
    pub code: Option<u32>,
    pub message: String,
}
impl NativeError {
    pub fn unsupported(source: &str) -> Self {
        Self {
            source: source.into(),
            code: None,
            message: "此诊断数据源仅支持 Windows。".into(),
        }
    }
    pub fn denied(&self) -> bool {
        matches!(self.code, Some(5 | 0x80070005 | 0x80041003))
    }
}
impl std::fmt::Display for NativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}：{}", self.source, self.message)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProcessSample {
    pub pid: u32,
    pub name: String,
    pub created: u64,
    pub cpu_100ns: u64,
    pub working_set: Option<u64>,
    pub path: Option<String>,
    pub read_bytes: Option<u64>,
    pub write_bytes: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct NativeOverview {
    pub cpu_total_pct: Option<f64>,
    pub logical_cores: Option<u32>,
    pub physical_cores: Option<u32>,
    pub total_mb: Option<f64>,
    pub free_mb: Option<f64>,
    pub used_mb: Option<f64>,
    pub used_pct: Option<f64>,
    pub nonpaged_mb: Option<f64>,
    pub paged_mb: Option<f64>,
    pub pagefile: Option<Vec<Value>>,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub cpu: Option<String>,
    pub bios: Option<String>,
    pub bios_date: Option<String>,
    pub uptime: Option<Value>,
    pub errors: Vec<NativeError>,
}
#[derive(Clone, Debug)]
pub struct SpaceSample {
    pub drive: String,
    pub total: u64,
    pub free: u64,
}
#[derive(Default)]
pub struct SpaceSnapshot {
    pub rows: Vec<SpaceSample>,
    pub errors: Vec<NativeError>,
}
#[derive(Clone, Debug, Default)]
pub struct VolumeSnapshot {
    pub rows: Vec<Value>,
    pub errors: Vec<NativeError>,
}
#[derive(Clone, Debug)]
pub struct NativeHealthRow {
    pub row: Value,
    pub controller_scope: bool,
}
impl NativeHealthRow {
    pub fn device(row: Value) -> Self {
        Self {
            row,
            controller_scope: false,
        }
    }
}
#[derive(Default)]
pub struct StartupSnapshot {
    pub reg_items: Vec<Value>,
    pub folders: Vec<Value>,
    pub errors: Vec<NativeError>,
}
#[derive(Clone, Debug)]
pub struct EventQuery {
    pub log: String,
    pub xpath: String,
    pub max_events: usize,
}
#[derive(Clone, Debug, Default)]
pub struct EventRecord {
    pub log: String,
    pub time: String,
    pub record_id: u64,
    pub level: u8,
    pub provider: String,
    pub id: u16,
    pub machine: Option<String>,
    pub message: Option<String>,
}
#[derive(Default)]
pub struct EventRead {
    pub unreadable: usize,
    pub truncated: bool,
}
pub type EventVisitor<'a> = dyn FnMut(EventRecord) -> Result<(), String> + 'a;
/// The native formatter and its handles remain on the diagnostic worker thread.
pub type EventMessage = Box<dyn FnOnce() -> Option<String>>;
pub type LazyEventVisitor<'a> = dyn FnMut(EventRecord, EventMessage) -> Result<(), String> + 'a;

/// The only platform seam. Collectors own projection/filtering; sources own read-only OS calls.
pub trait DiagnosticSource: Send + Sync {
    fn query(
        &self,
        namespace: &str,
        wql: &str,
        control: &Control,
    ) -> Result<Vec<Value>, NativeError>;
    fn processes(&self, control: &Control) -> Result<Vec<ProcessSample>, NativeError>;
    /// Populate executable image paths only for the final Top-N PIDs using a reused buffer.
    fn fill_process_paths(&self, _pids: &mut [(u32, Option<String>)]) {}
    /// Fast Win32 native overview snapshot (memory, kernel pool, uptime, CPU short dual-sample,
    /// pagefiles, registry machine/BIOS). Returning `Ok(None)` falls back to WMI.
    fn native_overview(&self, _control: &Control) -> Result<Option<NativeOverview>, NativeError> {
        Ok(None)
    }
    /// Fast PID -> process image name map (`CreateToolhelp32Snapshot`), avoiding WMI `Win32_Process`.
    fn native_process_names(
        &self,
        _control: &Control,
    ) -> Result<Option<std::collections::HashMap<u64, String>>, NativeError> {
        Ok(None)
    }
    /// Fast Win32 SCM auto-start service listing (`EnumServicesStatusExW` + `QueryServiceConfigW`).
    fn native_auto_services(&self, _control: &Control) -> Result<Option<Vec<Value>>, NativeError> {
        Ok(None)
    }
    /// Fast Win32 SCM status lookup for named services (`bthserv`, `Audiosrv`, etc.).
    fn native_services_by_name(
        &self,
        _names: &[&str],
        _control: &Control,
    ) -> Result<Option<Vec<Value>>, NativeError> {
        Ok(None)
    }
    /// Fast SetupAPI signed driver listing for the requested device classes.
    fn native_signed_drivers(
        &self,
        _classes: &[&str],
        _control: &Control,
    ) -> Result<Option<Vec<Value>>, NativeError> {
        Ok(None)
    }
    fn space(&self, drive: Option<&str>, control: &Control) -> Result<SpaceSnapshot, NativeError>;
    /// Native volume -> Windows physical disk number mapping (`IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`).
    /// Returning `Ok(None)` lets callers fall back to WMI.
    fn volume_disk_numbers(
        &self,
        _drive: &str,
        _control: &Control,
    ) -> Result<Option<Vec<u32>>, NativeError> {
        Ok(None)
    }
    /// Native logical volume enumeration matching `Win32_LogicalDisk` column keys plus per-volume errors.
    /// Returning `Ok(None)` lets callers fall back to WMI.
    fn native_volumes(
        &self,
        _drive: Option<&str>,
        _control: &Control,
    ) -> Result<Option<VolumeSnapshot>, NativeError> {
        Ok(None)
    }
    /// Native physical disk enumeration fallback (e.g. WinPE without Storage WMI),
    /// matching `MSFT_PhysicalDisk` column keys.
    fn native_physical_disks(&self, _control: &Control) -> Result<Option<Vec<Value>>, NativeError> {
        Ok(None)
    }
    /// Native SMART / reliability probe for a physical disk number (`\\.\PhysicalDriveN`),
    /// optionally cross-checking `expected_serial` before returning a 6-field `API.md` SMART row.
    /// Returning `Ok(None)` means native IOCTL is unsupported or identity did not match -> fall back to batch WMI.
    fn native_disk_health(
        &self,
        _disk_number: u32,
        _device_id: &str,
        _expected_serial: Option<&str>,
        _control: &Control,
    ) -> Result<Option<NativeHealthRow>, NativeError> {
        Ok(None)
    }
    fn startup(&self, control: &Control) -> Result<StartupSnapshot, NativeError>;
    fn admin(&self) -> Result<bool, NativeError>;
    fn events(
        &self,
        query: &EventQuery,
        visitor: &mut EventVisitor<'_>,
        control: &Control,
    ) -> Result<EventRead, NativeError>;
    fn events_lazy(
        &self,
        query: &EventQuery,
        visitor: &mut LazyEventVisitor<'_>,
        control: &Control,
    ) -> Result<EventRead, NativeError> {
        self.events(
            query,
            &mut |mut event| {
                let message = event.message.take();
                visitor(event, Box::new(move || message))
            },
            control,
        )
    }
}

#[cfg(not(windows))]
impl DiagnosticSource for WindowsSource {
    fn query(&self, _: &str, _: &str, _: &Control) -> Result<Vec<Value>, NativeError> {
        Err(NativeError::unsupported("WMI"))
    }
    fn processes(&self, _: &Control) -> Result<Vec<ProcessSample>, NativeError> {
        Err(NativeError::unsupported("Process"))
    }
    fn space(&self, _: Option<&str>, _: &Control) -> Result<SpaceSnapshot, NativeError> {
        Err(NativeError::unsupported("Storage"))
    }
    fn startup(&self, _: &Control) -> Result<StartupSnapshot, NativeError> {
        Err(NativeError::unsupported("Registry"))
    }
    fn admin(&self) -> Result<bool, NativeError> {
        Err(NativeError::unsupported("Token"))
    }
    fn events(
        &self,
        _: &EventQuery,
        _: &mut EventVisitor<'_>,
        _: &Control,
    ) -> Result<EventRead, NativeError> {
        Err(NativeError::unsupported("EventLog"))
    }
}

pub(crate) fn number(row: &Value, key: &str) -> Option<f64> {
    row[key]
        .as_f64()
        .or_else(|| row[key].as_str()?.parse().ok())
        .filter(|v| v.is_finite())
}
pub(crate) fn rounded(n: f64, digits: i32) -> f64 {
    let scale = 10_f64.powi(digits);
    (n * scale).round() / scale
}
pub(crate) fn selected(row: &Value, pairs: &[(&str, &str)]) -> Value {
    Value::Object(
        pairs
            .iter()
            .map(|(to, from)| (to.to_string(), row[*from].clone()))
            .collect(),
    )
}
pub(crate) fn add_errors(value: &mut Value, errors: Vec<NativeError>) {
    if !errors.is_empty() {
        value["degraded"] = Value::Bool(true);
        value["collectionErrors"] = serde_json::to_value(errors).unwrap_or_default();
    }
}
pub(crate) fn format_dmtf_date(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.len() == 10
        && trimmed.as_bytes()[4] == b'-'
        && trimmed.as_bytes()[7] == b'-'
        && trimmed[..4].bytes().all(|b| b.is_ascii_digit())
    {
        return Some(trimmed.to_owned());
    }
    if trimmed.len() >= 8 && trimmed[..8].bytes().all(|b| b.is_ascii_digit()) {
        let y = &trimmed[0..4];
        let m = &trimmed[4..6];
        let d = &trimmed[6..8];
        let (mi, di): (u32, u32) = (m.parse().ok()?, d.parse().ok()?);
        if (1..=12).contains(&mi) && (1..=31).contains(&di) {
            return Some(format!("{y}-{m}-{d}"));
        }
    }
    // Registry BIOSReleaseDate often uses `MM/DD/YYYY`.
    let parts: Vec<&str> = trimmed.split('/').collect();
    if parts.len() == 3 {
        let (m, d, y): (u32, u32, u32) = (
            parts[0].parse().ok()?,
            parts[1].parse().ok()?,
            parts[2].parse().ok()?,
        );
        if (1900..=2200).contains(&y) && (1..=12).contains(&m) && (1..=31).contains(&d) {
            return Some(format!("{y:04}-{m:02}-{d:02}"));
        }
    }
    None
}

pub(crate) fn collect(
    source: &dyn DiagnosticSource,
    namespace: &str,
    wql: &str,
    control: &Control,
    errors: &mut Vec<NativeError>,
) -> Vec<Value> {
    match source.query(namespace, wql, control) {
        Ok(rows) => rows,
        Err(error) => {
            errors.push(error);
            Vec::new()
        }
    }
}

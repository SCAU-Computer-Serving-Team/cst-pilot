use super::{
    Control, DiagnosticSource, EventQuery, EventRead, EventVisitor, LazyEventVisitor, NativeError,
    NativeHealthRow, NativeOverview, ProcessSample, SpaceSnapshot, StartupSnapshot, VolumeSnapshot,
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{cell::RefCell, collections::HashMap};
use windows::Win32::Foundation::{CloseHandle, HANDLE, RPC_E_TOO_LATE};
use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoInitializeEx, CoInitializeSecurity, CoUninitialize, EOAC_NONE,
    RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE,
};
use wmi::{Variant, WMIConnection, WMIError};
mod events;
mod process;
mod registry;
mod storage;
mod system;

pub struct WindowsSource;
pub(super) fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}
pub(super) fn native_error(source: &str, error: windows::core::Error) -> NativeError {
    NativeError {
        source: source.into(),
        code: Some(error.code().0 as u32),
        message: error.message(),
    }
}
pub(super) fn stopped(source: &str, control: &Control) -> Result<(), NativeError> {
    control.check().map_err(|message| NativeError {
        source: source.into(),
        code: None,
        message,
    })
}
struct Com;
impl Com {
    fn new() -> Result<Self, NativeError> {
        // Each instance balances the successful COM initialization on this worker.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() }
            .map_err(|e| native_error("COM", e))?;
        let com = Self;
        static SECURITY: std::sync::OnceLock<Result<(), NativeError>> = std::sync::OnceLock::new();
        SECURITY
            .get_or_init(|| {
                match unsafe {
                    CoInitializeSecurity(
                        None,
                        -1,
                        None,
                        None,
                        RPC_C_AUTHN_LEVEL_DEFAULT,
                        RPC_C_IMP_LEVEL_IMPERSONATE,
                        None,
                        EOAC_NONE,
                        None,
                    )
                } {
                    Ok(()) => Ok(()),
                    Err(e) if e.code() == RPC_E_TOO_LATE => Ok(()),
                    Err(e) => Err(native_error("CoInitializeSecurity", e)),
                }
            })
            .clone()?;
        Ok(com)
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}
pub(super) struct Handle(pub HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
fn wmi_error(source: &str, error: WMIError) -> NativeError {
    let code = if let WMIError::HResultError { hres } = &error {
        Some(*hres as u32)
    } else {
        None
    };
    NativeError {
        source: source.into(),
        code,
        message: error.to_string(),
    }
}

#[derive(Default)]
struct WmiCache {
    runtime: Option<tokio::runtime::Runtime>,
    connections: HashMap<String, WMIConnection>,
    com: Option<Com>,
}

thread_local! {
    static WMI_CACHE: RefCell<Option<WmiCache>> = const { RefCell::new(None) };
}

pub(super) fn with_wmi_scope<T>(run: impl FnOnce() -> T) -> T {
    struct Restore(Option<WmiCache>);
    impl Drop for Restore {
        fn drop(&mut self) {
            WMI_CACHE.replace(self.0.take());
        }
    }
    let _restore = Restore(WMI_CACHE.replace(Some(WmiCache::default())));
    run()
}

impl WmiCache {
    fn query(
        &mut self,
        namespace: &str,
        wql: &str,
        control: &Control,
    ) -> Result<Vec<Value>, NativeError> {
        if self.com.is_none() {
            self.com = Some(Com::new()?);
        }
        if !self.connections.contains_key(namespace) {
            let connection = WMIConnection::with_namespace_path(namespace)
                .map_err(|e| wmi_error(namespace, e))?;
            self.connections.insert(namespace.into(), connection);
        }
        self.runtime()?;
        let connection = &self.connections[namespace];
        let runtime = self.runtime.as_ref().expect("WMI runtime initialized");
        Self::query_with(runtime, connection, wql, control)
    }

    fn runtime(&mut self) -> Result<&tokio::runtime::Runtime, NativeError> {
        if self.runtime.is_none() {
            self.runtime = Some(
                tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                    .map_err(|e| NativeError {
                        source: "WMI".into(),
                        code: None,
                        message: e.to_string(),
                    })?,
            );
        }
        Ok(self.runtime.as_ref().expect("WMI runtime initialized"))
    }

    fn query_with(
        runtime: &tokio::runtime::Runtime,
        connection: &WMIConnection,
        wql: &str,
        control: &Control,
    ) -> Result<Vec<Value>, NativeError> {
        runtime.block_on(async{
            let work=async{
                let mut stream=connection.exec_query_async(wql).map_err(|e|wmi_error(wql,e))?;
                let mut rows=Vec::new();
                while let Some(row)=stream.next().await{
                    stopped("WMI",control)?;
                    let row:HashMap<String,Variant>=row.and_then(|r|r.into_desr()).map_err(|e|wmi_error(wql,e))?;
                    rows.push(json!(row));
                    if rows.len()>100_000{return Err(NativeError{source:wql.into(),code:None,message:"WMI 返回记录超过采集上限。".into()});}
                }
                Ok(rows)
            };
            tokio::select!{
                _=control.cancel.cancelled()=>Err(NativeError{source:wql.into(),code:None,message:"诊断已取消。".into()}),
                _=tokio::time::sleep(control.remaining())=>Err(NativeError{source:wql.into(),code:None,message:"WMI 查询超时。".into()}),
                result=work=>result,
            }
        })
    }
}

impl DiagnosticSource for WindowsSource {
    fn query(
        &self,
        namespace: &str,
        wql: &str,
        control: &Control,
    ) -> Result<Vec<Value>, NativeError> {
        stopped("WMI", control)?;
        WMI_CACHE.with(|cache| match cache.borrow_mut().as_mut() {
            Some(cache) => cache.query(namespace, wql, control),
            None => WmiCache::default().query(namespace, wql, control),
        })
    }
    fn processes(&self, control: &Control) -> Result<Vec<ProcessSample>, NativeError> {
        process::snapshot(control)
    }
    fn fill_process_paths(&self, pids: &mut [(u32, u64, Option<String>)]) {
        process::fill_paths(pids);
    }
    fn native_overview(&self, control: &Control) -> Result<Option<NativeOverview>, NativeError> {
        system::overview(control).map(Some)
    }
    fn native_process_names(
        &self,
        control: &Control,
    ) -> Result<Option<HashMap<u64, String>>, NativeError> {
        process::process_names(control).map(Some)
    }
    fn native_auto_services(&self, control: &Control) -> Result<Option<Vec<Value>>, NativeError> {
        system::auto_services(control).map(Some)
    }
    fn native_services_by_name(
        &self,
        names: &[&str],
        control: &Control,
    ) -> Result<Option<Vec<Value>>, NativeError> {
        system::services_by_name(names, control).map(Some)
    }
    fn native_signed_drivers(
        &self,
        classes: &[&str],
        control: &Control,
    ) -> Result<Option<Vec<Value>>, NativeError> {
        system::signed_drivers(classes, control).map(Some)
    }
    fn space(&self, drive: Option<&str>, control: &Control) -> Result<SpaceSnapshot, NativeError> {
        storage::space(drive, control)
    }
    fn volume_disk_numbers(
        &self,
        drive: &str,
        control: &Control,
    ) -> Result<Option<Vec<u32>>, NativeError> {
        storage::volume_disk_numbers(drive, control)
    }
    fn native_volumes(
        &self,
        drive: Option<&str>,
        control: &Control,
    ) -> Result<Option<VolumeSnapshot>, NativeError> {
        storage::native_volumes(drive, control)
    }
    fn native_physical_disks(&self, control: &Control) -> Result<Option<Vec<Value>>, NativeError> {
        storage::native_physical_disks(control)
    }
    fn native_disk_health(
        &self,
        disk_number: u32,
        device_id: &str,
        expected_serial: Option<&str>,
        control: &Control,
    ) -> Result<Option<NativeHealthRow>, NativeError> {
        storage::native_disk_health(disk_number, device_id, expected_serial, control)
    }
    fn startup(&self, control: &Control) -> Result<StartupSnapshot, NativeError> {
        registry::snapshot(control)
    }
    fn admin(&self) -> Result<bool, NativeError> {
        cst_os::is_elevated().map_err(|message| NativeError {
            source: "token".into(),
            code: None,
            message,
        })
    }
    fn events(
        &self,
        query: &EventQuery,
        visitor: &mut EventVisitor<'_>,
        control: &Control,
    ) -> Result<EventRead, NativeError> {
        let mut missing_messages = 0;
        let mut read = self.events_lazy(
            query,
            &mut |mut event, message| {
                event.message = message();
                missing_messages += usize::from(event.message.is_none());
                visitor(event)
            },
            control,
        )?;
        read.unreadable += missing_messages;
        Ok(read)
    }
    fn events_lazy(
        &self,
        query: &EventQuery,
        visitor: &mut LazyEventVisitor<'_>,
        control: &Control,
    ) -> Result<EventRead, NativeError> {
        events::query(query, visitor, control)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wmi_runtime_is_reused_within_scope_and_dropped_afterwards() {
        assert!(WMI_CACHE.with(|cache| cache.borrow().is_none()));
        with_wmi_scope(|| {
            let runtime = WMI_CACHE.with(|cell| {
                let mut cell = cell.borrow_mut();
                let cache = cell.as_mut().unwrap();
                cache.runtime().unwrap() as *const _
            });
            WMI_CACHE.with(|cell| {
                let mut cell = cell.borrow_mut();
                assert_eq!(
                    cell.as_mut().unwrap().runtime().unwrap() as *const _,
                    runtime
                );
            });
        });
        assert!(WMI_CACHE.with(|cache| cache.borrow().is_none()));
    }
}

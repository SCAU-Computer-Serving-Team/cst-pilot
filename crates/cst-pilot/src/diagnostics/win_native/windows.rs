use super::{
    Control, DiagnosticSource, EventQuery, EventRead, EventVisitor, NativeError, ProcessSample,
    SpaceSample, SpaceSnapshot, StartupSnapshot,
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use windows::Win32::Foundation::{CloseHandle, HANDLE, RPC_E_TOO_LATE};
use windows::Win32::Security::{
    CheckTokenMembership, CreateWellKnownSid, PSID, WinBuiltinAdministratorsSid,
};
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetLogicalDrives};
use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoInitializeEx, CoInitializeSecurity, CoUninitialize, EOAC_NONE,
    RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::core::{BOOL, PCWSTR};
use wmi::{Variant, WMIConnection, WMIError};
mod events;
mod process;
mod registry;

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

impl DiagnosticSource for WindowsSource {
    fn query(
        &self,
        namespace: &str,
        wql: &str,
        control: &Control,
    ) -> Result<Vec<Value>, NativeError> {
        stopped("WMI", control)?;
        let _com = Com::new()?;
        let connection =
            WMIConnection::with_namespace_path(namespace).map_err(|e| wmi_error(namespace, e))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|e| NativeError {
                source: "WMI".into(),
                code: None,
                message: e.to_string(),
            })?;
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
    fn processes(&self, control: &Control) -> Result<Vec<ProcessSample>, NativeError> {
        process::snapshot(control)
    }
    fn space(&self, drive: Option<&str>, control: &Control) -> Result<SpaceSnapshot, NativeError> {
        let drives = unsafe { GetLogicalDrives() };
        if drives == 0 {
            return Err(native_error(
                "GetLogicalDrives",
                windows::core::Error::from_thread(),
            ));
        }
        let mut rows = Vec::new();
        let mut errors = Vec::new();
        for index in 0..26u8 {
            stopped("Storage", control)?;
            let letter = (b'A' + index) as char;
            let name = format!("{letter}:");
            if drives & (1 << index) == 0 || drive.is_some_and(|d| d != name) {
                continue;
            }
            let path = wide(&format!("{name}\\"));
            let mut total = 0;
            let mut free = 0;
            match unsafe {
                GetDiskFreeSpaceExW(
                    PCWSTR(path.as_ptr()),
                    None,
                    Some(&mut total),
                    Some(&mut free),
                )
            } {
                Ok(()) => rows.push(SpaceSample {
                    drive: name,
                    total,
                    free,
                }),
                Err(e) => {
                    errors.push(native_error(&format!("GetDiskFreeSpaceExW({name})"), e));
                }
            }
        }
        if drive.is_some() && rows.is_empty() && errors.is_empty() {
            return Err(NativeError {
                source: "Storage".into(),
                code: None,
                message: "指定盘符不存在或无法读取。".into(),
            });
        }
        Ok(SpaceSnapshot { rows, errors })
    }
    fn startup(&self, control: &Control) -> Result<StartupSnapshot, NativeError> {
        registry::snapshot(control)
    }
    fn admin(&self) -> Result<bool, NativeError> {
        let mut buffer = [0u32; 17];
        let mut size = std::mem::size_of_val(&buffer) as u32;
        let sid = PSID(buffer.as_mut_ptr().cast());
        let mut member = BOOL::default();
        unsafe {
            CreateWellKnownSid(WinBuiltinAdministratorsSid, None, Some(sid), &mut size)
                .map_err(|e| native_error("CreateWellKnownSid", e))?;
            CheckTokenMembership(None, sid, &mut member)
                .map_err(|e| native_error("CheckTokenMembership", e))?;
        }
        Ok(member.as_bool())
    }
    fn events(
        &self,
        query: &EventQuery,
        visitor: &mut EventVisitor<'_>,
        control: &Control,
    ) -> Result<EventRead, NativeError> {
        events::query(query, visitor, control)
    }
}

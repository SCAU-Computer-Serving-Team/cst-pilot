//! Local native COM only. No shell or executable receives a recovery password.
use super::{core::BitLockerSource, model::*};
use crate::diagnostics::win_native::{Control, NativeError};

pub struct WindowsBitLocker;
#[cfg(not(windows))]
impl BitLockerSource for WindowsBitLocker {
    fn snapshot(&self, _: &Control) -> Result<Snapshot, NativeError> {
        Err(NativeError::unsupported("BitLocker"))
    }
    fn mutate(
        &self,
        _: &Volume,
        _: Scope,
        _: Option<&str>,
        _: &Control,
    ) -> Result<u32, NativeError> {
        Err(NativeError::unsupported("BitLocker"))
    }
}
#[cfg(windows)]
mod native {
    use super::*;
    use crate::diagnostics::win_native::{DiagnosticSource, WindowsSource};
    use std::sync::OnceLock;
    use windows::{
        Win32::{
            Foundation::RPC_E_TOO_LATE,
            System::{
                Com::*,
                Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE},
                Variant::{VARIANT, VT_BSTR},
                Wmi::*,
            },
        },
        core::{BSTR, IUnknown, Interface, PCWSTR},
    };
    use wmi::{IWbemClassWrapper, Variant};
    const NAMESPACE: &str = r"ROOT\CIMV2\Security\MicrosoftVolumeEncryption";
    const CLASS: &str = "Win32_EncryptableVolume";
    struct Apartment;
    impl Apartment {
        fn new() -> Result<Self, NativeError> {
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() }
                .map_err(|e| error("CoInitializeEx", e))?;
            let guard = Self;
            static SECURITY: OnceLock<Result<(), NativeError>> = OnceLock::new();
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
                        Err(e) => Err(error("CoInitializeSecurity", e)),
                    }
                })
                .clone()?;
            Ok(guard)
        }
    }
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() }
        }
    }
    fn error(source: &str, e: windows::core::Error) -> NativeError {
        // Never include a COM description: it may echo method input.
        NativeError {
            source: source.into(),
            code: Some(e.code().0 as u32),
            message: "Windows 原生接口调用失败".into(),
        }
    }
    fn missing(source: &str) -> NativeError {
        NativeError {
            source: source.into(),
            code: None,
            message: "Windows 方法缺少有效字段".into(),
        }
    }
    fn check(control: &Control) -> Result<(), NativeError> {
        control.check().map_err(|_| NativeError {
            source: CLASS.into(),
            code: None,
            message: "BitLocker 调用已取消或超时".into(),
        })
    }
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }
    fn secure(proxy: impl windows::core::Param<IUnknown>) -> Result<(), NativeError> {
        // Pass the original interface proxy. QueryInterface(IUnknown) can select
        // a different proxy whose blanket would not protect this interface.
        unsafe {
            CoSetProxyBlanket(
                proxy,
                RPC_C_AUTHN_WINNT,
                RPC_C_AUTHZ_NONE,
                None,
                RPC_C_AUTHN_LEVEL_PKT_PRIVACY,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            )
        }
        .map_err(|e| error("PacketPrivacy", e))
    }
    struct Connection {
        svc: IWbemServices,
        _apartment: Apartment,
    }
    impl Connection {
        fn new(control: &Control) -> Result<Self, NativeError> {
            check(control)?;
            let apartment = Apartment::new()?;
            let locator: IWbemLocator =
                unsafe { CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER) }
                    .map_err(|e| error("WbemLocator", e))?;
            let empty = BSTR::new();
            let svc = unsafe {
                locator.ConnectServer(
                    &BSTR::from(NAMESPACE),
                    &empty,
                    &empty,
                    &empty,
                    WBEM_FLAG_CONNECT_USE_MAX_WAIT.0,
                    &empty,
                    None,
                )
            }
            .map_err(|e| error(NAMESPACE, e))?;
            secure(&svc)?;
            check(control)?;
            Ok(Self {
                svc,
                _apartment: apartment,
            })
        }
        fn instances(&self, control: &Control) -> Result<Vec<IWbemClassObject>, NativeError> {
            check(control)?;
            let rows = unsafe { self.svc.ExecQuery(&BSTR::from("WQL"), &BSTR::from("SELECT DeviceID, DriveLetter, VolumeType, __PATH FROM Win32_EncryptableVolume"), WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY | WBEM_FLAG_ENSURE_LOCATABLE, None) }.map_err(|e| error(CLASS,e))?;
            secure(&rows)?;
            let mut result = Vec::new();
            loop {
                check(control)?;
                let mut row = [None];
                let mut returned = 0;
                let status = unsafe { rows.Next(20, &mut row, &mut returned) };
                status.ok().map_err(|e| error(CLASS, e))?;
                if let Some(row) = row[0].take() {
                    result.push(row);
                }
                if result.len() > 256 {
                    return Err(NativeError {
                        source: CLASS.into(),
                        code: None,
                        message: "卷数量超过采集上限".into(),
                    });
                }
                if returned == 0 && status.0 != WBEM_S_TIMEDOUT.0 {
                    break;
                }
            }
            Ok(result)
        }
        fn method(
            &self,
            path: &str,
            method: &str,
            args: &[(&str, VARIANT)],
            control: &Control,
            mutation: bool,
        ) -> Result<IWbemClassObject, NativeError> {
            check(control)?;
            let mut class = None;
            unsafe {
                self.svc.GetObject(
                    &BSTR::from(CLASS),
                    WBEM_GENERIC_FLAG_TYPE(0),
                    None,
                    Some(&mut class),
                    None,
                )
            }
            .map_err(|e| error(method, e))?;
            let class = class.ok_or_else(|| missing(method))?;
            let name = wide(method);
            let mut signature = None;
            unsafe {
                class.GetMethod(
                    PCWSTR(name.as_ptr()),
                    0,
                    &mut signature,
                    std::ptr::null_mut(),
                )
            }
            .map_err(|e| error(method, e))?;
            let parameters = signature
                .map(|s| unsafe { s.SpawnInstance(0) })
                .transpose()
                .map_err(|e| error(method, e))?;
            if !args.is_empty() && parameters.is_none() {
                return Err(missing(method));
            }
            // IWbemClassObject owns an internal COM copy. Clear its property on
            // every exit; Windows owns any additional RPC marshaling buffers.
            let parameters = Parameters(parameters);
            for (key, value) in args {
                let key = wide(key);
                unsafe {
                    parameters.0.as_ref().ok_or_else(|| missing(method))?.Put(
                        PCWSTR(key.as_ptr()),
                        0,
                        value,
                        0,
                    )
                }
                .map_err(|e| error(method, e))?;
            }
            check(control)?;
            let mut call = None;
            unsafe {
                self.svc.ExecMethod(
                    &BSTR::from(path),
                    &BSTR::from(method),
                    WBEM_FLAG_RETURN_IMMEDIATELY,
                    None,
                    parameters.0.as_ref(),
                    None,
                    Some(&mut call),
                )
            }
            .map_err(|e| error(method, e))?;
            // ExecMethod has marshaled its input before returning the call handle.
            // Remove the local COM object's credential while the method is pending.
            drop(parameters);
            let call = call.ok_or_else(|| missing(method))?;
            // For a submitted mutation, do not abandon the provider call when
            // the caller's 60-second wait expires. Its worker still owns the lock.
            let privacy_error = secure(&call).err();
            if !mutation && let Some(error) = privacy_error {
                return Err(error);
            }
            loop {
                if !mutation {
                    check(control)?;
                }
                // Preserve WBEM_S_TIMEDOUT, a success HRESULT with a null object.
                // The generated Result<T> wrapper would lose that distinction.
                let mut raw = std::ptr::null_mut();
                let status =
                    unsafe { (call.vtable().GetResultObject)(call.as_raw(), 20, &mut raw) };
                if status.0 == WBEM_S_TIMEDOUT.0 {
                    continue;
                }
                status.ok().map_err(|e| error(method, e))?;
                if raw.is_null() {
                    return Err(missing(method));
                }
                let out = unsafe { IWbemClassObject::from_raw(raw) };
                return if let Some(error) = privacy_error {
                    Err(error)
                } else {
                    Ok(out)
                };
            }
        }
        fn read(
            &self,
            path: &str,
            method: &str,
            args: &[(&str, VARIANT)],
            control: &Control,
        ) -> Result<IWbemClassObject, NativeError> {
            let out = self.method(path, method, args, control, false)?;
            let code = number(&out, "ReturnValue").ok_or_else(|| missing(method))?;
            if code != 0 {
                return Err(NativeError {
                    source: method.into(),
                    code: Some(code),
                    message: "Windows 方法返回失败".into(),
                });
            }
            Ok(out)
        }
        fn volume(&self, row: &IWbemClassObject, control: &Control) -> Result<Volume, NativeError> {
            check(control)?;
            let id = text(row, "DeviceID").ok_or_else(|| missing("DeviceID"))?;
            normalize_id(&id).map_err(|_| missing("DeviceID"))?;
            let path = text(row, "__PATH").ok_or_else(|| missing("__PATH"))?;
            let mut v = Volume {
                volume_id: id,
                drive: text(row, "DriveLetter"),
                volume_type: number(row, "VolumeType"),
                ..Volume::default()
            };
            for (method, args, fields) in [
                (
                    "GetConversionStatus",
                    vec![("PrecisionFactor", VARIANT::from(0i32))],
                    vec!["ConversionStatus", "EncryptionPercentage"],
                ),
                ("GetEncryptionMethod", vec![], vec!["EncryptionMethod"]),
                ("GetProtectionStatus", vec![], vec!["ProtectionStatus"]),
                ("GetLockStatus", vec![], vec!["LockStatus"]),
            ] {
                match self.read(&path, method, &args, control) {
                    Ok(out) => {
                        for field in fields {
                            let value = number(&out, field);
                            if value.is_none() {
                                v.collection_errors.push(missing(method));
                            }
                            match field {
                                "ConversionStatus" => v.conversion_status = value,
                                "EncryptionPercentage" => v.encryption_percentage = value,
                                "EncryptionMethod" => v.encryption_method = value,
                                "ProtectionStatus" => v.protection_status = value,
                                "LockStatus" => v.lock_status = value,
                                _ => {}
                            }
                        }
                    }
                    Err(e) => v.collection_errors.push(e),
                }
            }
            if let Some(kind) = v.volume_type {
                let method = if kind == 0 {
                    "IsAutoUnlockKeyStored"
                } else {
                    "IsAutoUnlockEnabled"
                };
                match self.read(&path, method, &[], control) {
                    Ok(out) => {
                        let value = boolean(&out, method);
                        if value.is_none() {
                            v.collection_errors.push(missing(method));
                        }
                        if kind == 0 {
                            v.auto_unlock_keys_stored = value;
                        } else {
                            v.auto_unlock_enabled = value;
                        }
                    }
                    Err(e) => v.collection_errors.push(e),
                }
            }
            match self.read(
                &path,
                "GetKeyProtectors",
                &[("KeyProtectorType", VARIANT::from(0i32))],
                control,
            ) {
                Ok(out) => match property(&out, "VolumeKeyProtectorID") {
                    Some(Variant::Array(ids)) if ids.len() <= 256 => {
                        for id in ids {
                            let Variant::String(id) = id else {
                                v.collection_errors.push(missing("GetKeyProtectors"));
                                continue;
                            };
                            let kind = match self.read(
                                &path,
                                "GetKeyProtectorType",
                                &[("VolumeKeyProtectorID", VARIANT::from(id.as_str()))],
                                control,
                            ) {
                                Ok(out) => {
                                    let kind = number(&out, "KeyProtectorType");
                                    if kind.is_none() {
                                        v.collection_errors.push(missing("GetKeyProtectorType"));
                                    }
                                    kind
                                }
                                Err(e) => {
                                    v.collection_errors.push(e);
                                    None
                                }
                            };
                            v.key_protectors.push(Protector { id, r#type: kind });
                        }
                    }
                    _ => v.collection_errors.push(missing("GetKeyProtectors")),
                },
                Err(e) => v.collection_errors.push(e),
            }
            Ok(v)
        }
    }
    struct Parameters(Option<IWbemClassObject>);
    impl Drop for Parameters {
        fn drop(&mut self) {
            if let Some(params) = &self.0 {
                let key = wide("NumericalPassword");
                let empty = VARIANT::from("");
                let _ = unsafe { params.Put(PCWSTR(key.as_ptr()), 0, &empty, 0) };
            }
        }
    }
    struct SecretVariant(Vec<(&'static str, VARIANT)>);
    impl SecretVariant {
        fn new(password: &str) -> Self {
            Self(vec![("NumericalPassword", VARIANT::from(password))])
        }
        fn wipe(&mut self) {
            // This VARIANT exclusively owns its VT_BSTR buffer. Wipe exactly its
            // UTF-16 code units before VariantClear frees it. No casts of Rust strings.
            unsafe {
                let inner = &mut self.0[0].1.Anonymous.Anonymous;
                if inner.vt == VT_BSTR {
                    let bstr = &inner.Anonymous.bstrVal;
                    let ptr = bstr.as_ptr().cast_mut();
                    for i in 0..bstr.len() {
                        std::ptr::write_volatile(ptr.add(i), 0);
                    }
                    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
                }
            }
        }
    }
    impl Drop for SecretVariant {
        fn drop(&mut self) {
            self.wipe();
        }
    }
    fn property(row: &IWbemClassObject, key: &str) -> Option<Variant> {
        IWbemClassWrapper::new(row.clone()).get_property(key).ok()
    }
    fn text(row: &IWbemClassObject, key: &str) -> Option<String> {
        match property(row, key)? {
            Variant::String(s) if !s.is_empty() => Some(s),
            _ => None,
        }
    }
    fn number(row: &IWbemClassObject, key: &str) -> Option<u32> {
        match property(row, key)? {
            Variant::UI4(n) => Some(n),
            Variant::I4(n) => Some(n as u32),
            Variant::UI8(n) => u32::try_from(n).ok(),
            Variant::I8(n) => u32::try_from(n).ok(),
            _ => None,
        }
    }
    fn boolean(row: &IWbemClassObject, key: &str) -> Option<bool> {
        match property(row, key)? {
            Variant::Bool(b) => Some(b),
            _ => None,
        }
    }
    impl BitLockerSource for WindowsBitLocker {
        fn snapshot(&self, control: &Control) -> Result<Snapshot, NativeError> {
            let connection = Connection::new(control)?;
            let mut result = Snapshot {
                collected_at: chrono::Utc::now()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                admin: WindowsSource.admin()?,
                ..Snapshot::default()
            };
            for row in connection.instances(control)? {
                match connection.volume(&row, control) {
                    Ok(volume) => result.volumes.push(volume),
                    Err(e) => result.collection_errors.push(e),
                }
            }
            check(control)?;
            Ok(result)
        }
        fn mutate(
            &self,
            target: &Volume,
            scope: Scope,
            password: Option<&str>,
            control: &Control,
        ) -> Result<u32, NativeError> {
            if !matches!(scope, Scope::Unlock | Scope::Decrypt) {
                return Err(missing("scope"));
            }
            let connection = Connection::new(control)?;
            let rows: Vec<_> = connection
                .instances(control)?
                .into_iter()
                .filter(|row| {
                    text(row, "DeviceID").and_then(|id| normalize_id(&id).ok())
                        == normalize_id(&target.volume_id).ok()
                })
                .collect();
            if rows.len() != 1 {
                return Err(missing("目标卷"));
            }
            let row = &rows[0];
            let actual = connection.volume(row, control)?;
            if !WindowsSource.admin()?
                || actual.drive != target.drive
                || actual.reason(scope).is_some()
            {
                return Err(missing("执行条件已变化"));
            }
            let path = text(row, "__PATH").ok_or_else(|| missing("__PATH"))?;
            let out = if scope == Scope::Unlock {
                let secret = SecretVariant::new(password.ok_or_else(|| missing("恢复密码"))?);
                // Borrow the owning variant; do not Clone (which would create an untracked BSTR).
                connection.method(
                    &path,
                    "UnlockWithNumericalPassword",
                    &secret.0,
                    control,
                    true,
                )
            } else {
                connection.method(&path, "Decrypt", &[], control, true)
            }?;
            number(&out, "ReturnValue").ok_or_else(|| missing("ReturnValue"))
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn secret_bstr_is_overwritten_in_place_without_touching_other_variants() {
            let mut secret = SecretVariant::new("123456-234567");
            let other = VARIANT::from("unrelated");
            secret.wipe();
            unsafe {
                assert!(
                    secret.0[0]
                        .1
                        .Anonymous
                        .Anonymous
                        .Anonymous
                        .bstrVal
                        .iter()
                        .all(|c| *c == 0)
                );
            }
            assert!(
                matches!(Variant::from_variant(&other).unwrap(), Variant::String(s) if s == "unrelated")
            );
        }
    }
}

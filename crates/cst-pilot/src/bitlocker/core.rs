use super::{
    credentials::{Vault, redact, validate},
    lock::VolumeLock,
    model::*,
};
use crate::diagnostics::win_native::{Control, NativeError};
use pi_session::{ExtensionUiContext, UiDialogOptions};
use serde_json::{Value, json};
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

/// Only this seam reaches COM. Implementations must not return credential text.
/// A mutation returns after the submitted native method ends, including when its
/// caller has stopped waiting. This keeps the worker-owned volume lock alive.
pub trait BitLockerSource: Send + Sync {
    fn snapshot(&self, control: &Control) -> Result<Snapshot, NativeError>;
    fn mutate(
        &self,
        target: &Volume,
        scope: Scope,
        password: Option<&str>,
        control: &Control,
    ) -> Result<u32, NativeError>;
}
#[derive(Clone)]
pub struct Executor {
    source: Arc<dyn BitLockerSource>,
    timeout: Duration,
}
impl Executor {
    pub fn new(source: Arc<dyn BitLockerSource>) -> Self {
        Self {
            source,
            timeout: Duration::from_secs(60),
        }
    }
    pub fn with_timeout(source: Arc<dyn BitLockerSource>, timeout: Duration) -> Self {
        Self { source, timeout }
    }
    pub async fn snapshot(&self, cancel: CancellationToken) -> Result<Snapshot, String> {
        let source = self.source.clone();
        worker(cancel, self.timeout, move |control| {
            source.snapshot(&control).map_err(safe_error)
        })
        .await
    }
    /// Crash reconciliation only queries actual state; it never replays a command.
    pub async fn verify_interrupted(&self, cancel: CancellationToken) -> Result<Value, String> {
        let mut snapshot = self.snapshot(cancel).await?;
        for volume in &mut snapshot.volumes {
            volume.operation = Some(Operation::new(
                "unknown",
                "上次操作异常中断；已实时复查，未重试操作",
            ));
        }
        let mut result = finish(snapshot, Scope::Status)?;
        result["bitlocker"]["operation"] = json!(Operation::new(
            "unknown",
            "上次操作异常中断；已实时复查，未重试操作"
        ));
        Ok(result)
    }
    pub async fn reconcile(
        &self,
        report: &pi_session::execution_log::StartupReconciliationReport,
        cancel: CancellationToken,
    ) -> Option<Value> {
        if !report
            .unresolved_tool_calls
            .iter()
            .any(|call| call.tool_name == "bitlocker" && call.requires_state_verification)
        {
            return None;
        }
        Some(self.verify_interrupted(cancel).await.unwrap_or_else(|_| json!({"bitlocker":{"scope":"status","operation":{"status":"unknown","reason":"上次操作异常中断；实时查询失败，请重新查询。"},"degraded":true}})))
    }
    pub async fn execute(
        &self,
        id: &str,
        mut input: Input,
        vault: &Vault,
        ui: Arc<dyn ExtensionUiContext>,
        cancel: CancellationToken,
    ) -> Result<Value, String> {
        // Take ownership before validation, including error paths.
        let provided = input.recovery_password.take().map(Zeroizing::new);
        let drive = normalize_drive(input.drive.as_deref())?;
        if input.scope == Scope::Unlock && drive.is_none() {
            return Err("unlock 必须指定 drive。".into());
        }
        if input.scope != Scope::Unlock && provided.is_some() {
            return Err("recoveryPassword 仅用于 unlock。".into());
        }
        let mut provided = vault
            .claim(id, input.scope.name(), drive.as_deref(), provided)?
            .map(validate)
            .transpose()?;
        let mut result = self.snapshot(cancel.child_token()).await?;
        result.volumes.retain(|v| {
            drive
                .as_ref()
                .is_none_or(|d| v.drive.as_ref().is_some_and(|v| v.eq_ignore_ascii_case(d)))
        });
        if drive.is_some() && result.volumes.len() != 1 {
            return Err("指定盘符未找到唯一的可查询 BitLocker 卷。".into());
        }
        if result.volumes.is_empty() && !result.collection_errors.is_empty() {
            return Err("BitLocker 查询失败，请检查 collectionErrors 或系统权限。".into());
        }
        if input.scope == Scope::Status {
            return finish(result, input.scope);
        }
        for volume in &mut result.volumes {
            volume.operation = volume.reason(input.scope);
        }
        let targets: Vec<Volume> = result
            .volumes
            .iter()
            .filter(|v| v.operation.is_none())
            .cloned()
            .collect();
        if targets.is_empty() {
            return finish(result, input.scope);
        }
        let rejection = if !result.admin {
            Some("需要管理员权限")
        } else if !ui.has_ui() {
            Some("执行操作需要本地交互")
        } else {
            None
        };
        if let Some(reason) = rejection {
            mark_remaining(&mut result, "rejected", reason);
            return finish(result, input.scope);
        }
        if input.scope == Scope::Decrypt {
            let fixed = targets.iter().map(label).collect::<Vec<_>>().join("\n");
            if !confirm(
                &*ui,
                "确认完整解密",
                &format!(
                    "仅处理以下固定目标卷：\n{fixed}\nWindows 将在后台解密。请先确认备份和供电。"
                ),
                &cancel,
            )
            .await
            {
                mark_remaining(&mut result, "cancelled", "用户取消或确认等待已结束");
                return finish(result, input.scope);
            }
        }
        for i in 0..result.volumes.len() {
            if result.volumes[i].operation.is_some() {
                continue;
            }
            if cancel.is_cancelled() {
                mark_remaining(&mut result, "cancelled", "提交前已取消");
                break;
            }
            let target = result.volumes[i].clone();
            let lock = match VolumeLock::acquire(&target.volume_id) {
                Ok(lock) => Arc::new(lock),
                Err(reason) => {
                    result.volumes[i].operation = Some(Operation::new("rejected", &reason));
                    continue;
                }
            };
            let parameter = provided.is_some();
            let password = if input.scope == Scope::Unlock {
                let value = if let Some(password) = provided.take() {
                    if confirm(&*ui, "确认 AI 参数解锁", &format!("{}\n恢复密码已进入模型上下文，可能传送给模型服务商。是否使用本次参数解锁此卷？", label(&target)), &cancel).await { Some(password) } else { None }
                } else {
                    let title = format!("解锁 {}", label(&target));
                    let child = cancel.child_token();
                    let _guard = child.clone().drop_guard();
                    tokio::select! { biased;
                        _ = cancel.cancelled() => None,
                        value = tokio::time::timeout(Duration::from_secs(180), ui.input(&title, Some("八组六位数字，以连字符分隔"), Some(UiDialogOptions { secret: true, timeout_ms: Some(180_000), ..Default::default() }), child.clone())) => value.ok().flatten().map(Zeroizing::new),
                    }
                };
                let Some(password) = value else {
                    result.volumes[i].operation =
                        Some(Operation::new("cancelled", "用户取消或凭据等待已结束"));
                    continue;
                };
                Some(validate(password)?)
            } else {
                None
            };
            let _post_lock = lock.clone();
            let source = self.source.clone();
            let scope = input.scope;
            let original = target.clone();
            let timeout = self.timeout;
            let response = worker(cancel.child_token(), timeout, move |control| {
                let _lock = lock;
                let operation = (|| {
                    control.check()?;
                    let current = source.snapshot(&control).map_err(safe_error)?;
                    if !current.admin {
                        return Ok(Operation::new("rejected", "需要管理员权限"));
                    }
                    let matches: Vec<_> = current
                        .volumes
                        .iter()
                        .filter(|v| same_id(&v.volume_id, &original.volume_id))
                        .collect();
                    if matches.len() != 1 {
                        return Ok(Operation::new("rejected", "目标卷已不可用或身份不唯一"));
                    }
                    let volume = matches[0];
                    if volume.drive != original.drive {
                        return Ok(Operation::new("rejected", "目标卷盘符已变化"));
                    }
                    if let Some(reason) = volume.reason(scope) {
                        return Ok(reason);
                    }
                    control.check()?;
                    // The source also revalidates native object identity immediately before ExecMethod.
                    source
                        .mutate(
                            volume,
                            scope,
                            password.as_deref().map(String::as_str),
                            &control,
                        )
                        .map(|code| Operation::code(scope, code))
                        .or_else(|mut error| {
                            error.message = redact(&error.message);
                            Ok(Operation {
                                error: Some(error),
                                ..Operation::new(
                                    "unknown",
                                    "Windows 调用异常；已安排实时复查，未自动重试",
                                )
                            })
                        })
                })();
                // Even if the async caller is dropped after submission, this
                // worker performs a final real query while retaining its lock.
                let post = source
                    .snapshot(&Control::new(CancellationToken::new(), timeout))
                    .ok();
                operation.map(|operation| (operation, post))
            })
            .await;
            let (mut operation, post) = match response {
                Ok((operation, post)) => (operation, post),
                Err(_) => (
                    Operation::new("unknown", "请求可能已提交，未确认结果；未自动重试"),
                    self.snapshot(CancellationToken::new()).await.ok(),
                ),
            };
            if scope == Scope::Unlock {
                operation.credential_source = Some(
                    if parameter {
                        "toolParameter"
                    } else {
                        "localInput"
                    }
                    .into(),
                );
            }
            // A new independent token ensures cancellation of the operation does not
            // skip the mandatory real-time query. No old lock/conversion values survive.
            let mut current = target.unavailable();
            match post {
                Some(snapshot) => {
                    let mut matches = snapshot
                        .volumes
                        .into_iter()
                        .filter(|v| same_id(&v.volume_id, &target.volume_id));
                    if let Some(volume) = matches.next()
                        && matches.next().is_none()
                    {
                        current = volume;
                    }
                    result.collected_at = snapshot.collected_at;
                    result.collection_errors.extend(snapshot.collection_errors);
                }
                None => result.collection_errors.push(NativeError {
                    source: "postOperation".into(),
                    code: None,
                    message: "操作后实时查询失败，请重新查询。".into(),
                }),
            }
            current.operation = Some(operation);
            result.volumes[i] = current;
        }
        finish(result, input.scope)
    }
}
fn same_id(a: &str, b: &str) -> bool {
    normalize_id(a)
        .ok()
        .zip(normalize_id(b).ok())
        .is_some_and(|(a, b)| a == b)
}
fn safe_error(error: NativeError) -> String {
    format!("{}：{}", error.source, redact(&error.message))
}
fn label(v: &Volume) -> String {
    format!(
        "{} ({})",
        v.drive.as_deref().unwrap_or("无盘符"),
        v.volume_id
    )
}
fn mark_remaining(snapshot: &mut Snapshot, status: &str, reason: &str) {
    for volume in &mut snapshot.volumes {
        if volume.operation.is_none() {
            volume.operation = Some(Operation::new(status, reason));
        }
    }
}
async fn confirm(
    ui: &dyn ExtensionUiContext,
    title: &str,
    message: &str,
    cancel: &CancellationToken,
) -> bool {
    let child = cancel.child_token();
    let _guard = child.clone().drop_guard();
    tokio::select! { biased;
        _ = cancel.cancelled() => false,
        reply = tokio::time::timeout(Duration::from_secs(180), ui.confirm(title, message, Some(UiDialogOptions { timeout_ms: Some(180_000), ..Default::default() }), child.clone())) => reply.unwrap_or(false),
    }
}
fn finish(snapshot: Snapshot, scope: Scope) -> Result<Value, String> {
    let degraded = !snapshot.collection_errors.is_empty()
        || snapshot
            .volumes
            .iter()
            .any(|v| !v.collection_errors.is_empty());
    let mut value = serde_json::to_value(snapshot).map_err(|_| "BitLocker 状态编码失败。")?;
    let object = value.as_object_mut().ok_or("BitLocker 状态编码失败。")?;
    object.insert("scope".into(), json!(scope));
    object.insert("source".into(), json!("Win32_EncryptableVolume"));
    object.insert("degraded".into(), json!(degraded));
    object.insert("notice".into(), json!("状态来自 Win32_EncryptableVolume 本次枚举与实时方法查询；未出现的卷不代表未加密。encryptionPercentage 单位为 %；完成以 conversionStatus=0 为准。autoUnlockEnabled 仅适用于数据卷，autoUnlockKeysStored 仅适用于当前运行的系统卷。字段 null 须结合 collectionErrors 判断；请求结果与当前卷状态分别记录。"));
    // Defense at the native boundary: provider messages may contain arbitrary text.
    let safe = redact(&value.to_string());
    Ok(
        json!({"bitlocker": serde_json::from_str::<Value>(&safe).map_err(|_| "BitLocker 状态编码失败。")?}),
    )
}
async fn worker<T: Send + 'static>(
    cancel: CancellationToken,
    timeout: Duration,
    job: impl FnOnce(Control) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let pool = WORKERS.get_or_init(|| Arc::new(Semaphore::new(4))).clone();
    let control = Control::new(cancel.child_token(), timeout);
    let _guard = control.cancel.clone().drop_guard();
    let deadline = tokio::time::Instant::now() + timeout;
    let permit = tokio::select! { biased;
        _ = cancel.cancelled() => return Err("BitLocker 调用已取消。".into()),
        p = tokio::time::timeout_at(deadline, pool.acquire_owned()) => p.map_err(|_| "BitLocker 调用超时。")?.map_err(|_| "BitLocker 工作线程不可用。")?,
    };
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        job(control)
    });
    tokio::select! { biased;
        result = task => result.map_err(|_| "BitLocker 原生调用中断。")?,
        _ = cancel.cancelled() => Err("BitLocker 调用已取消。".into()),
        _ = tokio::time::sleep_until(deadline) => Err("BitLocker 调用超时。".into()),
    }
}

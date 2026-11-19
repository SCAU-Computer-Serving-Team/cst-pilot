//! Controlled BitLocker execution and recovery-password privacy.
pub mod core;
pub mod credentials;
pub mod lock;
pub mod model;
pub mod wmi_encryptable;

use crate::diagnostics::result::diagnostic_result;
pub use core::{BitLockerAnswer, BitLockerHostChannel, BitLockerPendingKind, Executor};
use credentials::{Vault, redact};
use pi_agent::{ToolExecutionMode, ToolReplayPolicy};
use pi_session::ToolDefinition;
use serde_json::json;
use std::sync::Arc;

pub const VERIFICATION_CUSTOM_TYPE: &str = "cst.bitlocker-state-verification";

/// Extracts the BitLocker progress stage emitted in a `ToolExecutionUpdate` details payload.
pub fn extract_progress_stage(details: &serde_json::Value) -> Option<&str> {
    details
        .get("bitlocker")
        .and_then(|b| b.get("stage"))
        .and_then(serde_json::Value::as_str)
}

/// Synthesizes a `ToolResultMessage` representing a startup BitLocker state verification
/// persisted under `VERIFICATION_CUSTOM_TYPE`.
pub fn synthesize_verification_result(
    tool_call_id: &str,
    data: &serde_json::Value,
    verified_at: u64,
) -> pi_ai::ToolResultMessage {
    let raw_text = serde_json::to_string(data).unwrap_or_default();
    pi_ai::ToolResultMessage {
        tool_call_id: tool_call_id.to_owned(),
        tool_name: "bitlocker".to_owned(),
        content: vec![pi_ai::UserContentBlock::Text(pi_ai::TextContent {
            text: raw_text,
            text_signature: None,
        })],
        details: Some(data.clone()),
        is_error: false,
        timestamp: verified_at,
        usage: None,
        nested_calls: None,
        duration_ms: None,
    }
}

pub fn definition(vault: Vault) -> ToolDefinition {
    definition_with(
        vault,
        Executor::new(Arc::new(wmi_encryptable::WindowsBitLocker)),
    )
}
pub fn definition_with(vault: Vault, executor: Executor) -> ToolDefinition {
    definition_with_host(vault, executor, None)
}
pub fn definition_with_host(
    vault: Vault,
    executor: Executor,
    host: Option<Arc<dyn BitLockerHostChannel>>,
) -> ToolDefinition {
    // A fixed error on argument conversion avoids echoing malformed credentials.
    let mut tool = ToolDefinition::new(
        "bitlocker",
        "BitLocker",
        "实时查询 BitLocker；指定盘符并在本地隐藏输入恢复密码解锁；本地确认固定目标后发起后台完整解密。",
        schemars::schema_for!(model::Input).to_value(),
        Arc::new(move |id, args, cancel, on_update, ctx| {
            let vault = vault.clone();
            let executor = executor.clone();
            let host = host.clone();
            Box::pin(async move {
                let input = serde_json::from_value(args).map_err(|_| "BitLocker 参数格式无效。")?;
                let stage_reporter = on_update.map(|sink| {
                    move |stage: &str| {
                        if let Ok(partial) =
                            diagnostic_result(json!({"bitlocker": {"stage": stage}}))
                        {
                            sink(partial);
                        }
                    }
                });
                let on_stage = stage_reporter
                    .as_ref()
                    .map(|f| f as &(dyn Fn(&str) + Send + Sync));
                let result = executor
                    .execute_with_host(
                        &id,
                        input,
                        &vault,
                        ctx.ui(),
                        host.as_deref(),
                        on_stage,
                        cancel.unwrap_or_default(),
                    )
                    .await
                    .map_err(|e| redact(&e))?;
                diagnostic_result(result)
            })
        }),
    );
    tool.execution_mode = Some(ToolExecutionMode::Sequential);
    tool.replay_policy = ToolReplayPolicy::Never;
    tool.prompt_snippet =
        Some("查询 BitLocker、使用本地恢复密码解锁，或经本地确认完整解密。".into());
    tool.prompt_guidelines = Some(vec![
        "unlock 或 decrypt 前先阅读 bitlocker 技能，仅在用户明确要求时执行。".into(),
        "默认省略 recoveryPassword，让用户在本地隐藏输入。AI 填参会进入模型上下文并可能传送给模型服务商。".into(),
        "缺少密钥时指引 https://aka.ms/myrecoverykey 并匹配密钥 ID；不索取微软账户密码或 Windows PIN。".into(),
        "仅 conversionStatus=0 确认完全解密。unknown 必须实时复查，禁止自动重放、自动解锁、删除保护器或调用 repair-bde。".into(),
    ]);
    tool
}

pub mod model;
mod plan_store;
pub(crate) mod storage;

use crate::diagnostics::result::diagnostic_result;
use model::RunbookToolInput;
use pi_session::ToolDefinition;
use std::path::Path;
use std::sync::Arc;
use storage::{Storage, cancelled};
use tokio_util::sync::CancellationToken;

pub use storage::DirectoryLock;

#[derive(Clone)]
pub struct RunbookStore {
    layout: cst_os::KitLayout,
    storage: Storage,
    root_state: Arc<crate::kit::KitRootState>,
}

impl RunbookStore {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, String> {
        let layout = cst_os::KitLayout::new(root.as_ref())?;
        let root_state = crate::kit::root_state_for(layout.root())?;
        Ok(Self {
            storage: Storage::new(layout.root())?,
            layout,
            root_state,
        })
    }

    async fn locked_plans<T, F>(&self, cancel: CancellationToken, task: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&Storage, &Path, &Storage, &CancellationToken) -> Result<T, String>
            + Send
            + 'static,
    {
        let activity = self.root_state.gate.enter()?;
        let writers = self.root_state.runbook_writes.clone();
        let child = cancel.child_token();
        let _cancel_on_drop = child.clone().drop_guard();
        let permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err("命令交付已取消。".into()),
            permit = writers.lock_owned() => permit,
        };
        let layout = self.layout.clone();
        let root_storage = self.storage.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _activity = activity;
            cancelled(&child)?;
            let (plan_storage, data_rel) = crate::kit::open_data_storage(&layout, true)?;
            let plans_rel = data_rel.join("plans");
            let _lease = plan_storage.pin_directory(&plans_rel, true)?;
            let dir = plan_storage.directory(&plans_rel, false)?;
            let _lock = DirectoryLock::acquire(&dir)?;
            cancelled(&child)?;
            task(&plan_storage, &plans_rel, &root_storage, &child)
        })
        .await
        .map_err(|_| "命令交付工作线程异常退出。".to_string())?
    }
}

pub fn is_runbook_tool(name: &str) -> bool {
    matches!(name, "runbook" | "command_plan" | "runbook_deliver")
}

pub fn is_runbook_delivery_tool(name: &str) -> bool {
    matches!(name, "runbook" | "runbook_deliver")
}

fn extract_runbook_object(message: &pi_ai::ToolResultMessage) -> Option<serde_json::Value> {
    if message.is_error {
        return None;
    }
    let details_val = message.details.clone().or_else(|| {
        let joined: String = message
            .content
            .iter()
            .filter_map(|part| match part {
                pi_ai::UserContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        serde_json::from_str::<serde_json::Value>(&joined).ok()
    })?;
    details_val.get("runbook").cloned()
}

/// Extracts the delivered/exported runbook text file path from a `runbook` or legacy `runbook_deliver` tool result.
pub fn extract_delivered_runbook_file(message: &pi_ai::ToolResultMessage) -> Option<String> {
    extract_runbook_object(message)?
        .get("file")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// Extracts `(runbookId, version)` from a saved `runbook` or legacy `command_plan` tool result.
pub fn extract_runbook_plan_ref(message: &pi_ai::ToolResultMessage) -> Option<(String, u64)> {
    let rb = extract_runbook_object(message)?;
    let id = rb.get("runbookId")?.as_str()?.to_owned();
    let version = rb.get("version")?.as_u64()?;
    Some((id, version))
}

pub fn definitions(root: &Path) -> Result<Vec<ToolDefinition>, String> {
    let store = RunbookStore::new(root)?;
    let mut runbook = ToolDefinition::from_typed(
        "runbook",
        "Runbook",
        "管理手动修复命令方案并按需导出 UTF-8 BOM + CRLF 纯文本清单。默认保存版本化方案（data/plans），传 exportTxt=true 或 action=\"export\" 导出至 artifacts/<日期>/runbook/<序号>-<风险档>-<标题>-v<版本>.txt。只写清单不执行命令。",
        move |_, input: RunbookToolInput, cancel, _, _| {
            let store = store.clone();
            async move { diagnostic_result(store.execute(input, cancel.unwrap_or_default()).await?) }
        },
    );
    runbook.prompt_snippet =
        Some("创建、修订手动修复命令方案，并在需要时导出为可读纯文本清单。".into());
    runbook.prompt_guidelines = Some(vec![
        "生成或修订修复命令前先阅读 app/skills/runbook/SKILL.md；合并同风险档命令，不写入诊断采集或破坏系统的操作，工具绝不自动执行命令。".into(),
        "新增或修订命令时调用 runbook（修订须传 runbookId、baseVersion 与完整 items）；默认 exportTxt=false 便于队员先核对并在卡片一键导出 TXT，若队员明确要求立即导出或处于无界面环境可传 exportTxt=true 或 action=\"export\"。".into(),
    ]);
    Ok(vec![runbook])
}

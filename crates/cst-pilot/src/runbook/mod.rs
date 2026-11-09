pub mod model;
mod plan_store;
pub mod sequence_store;
pub(crate) mod storage;

use crate::diagnostics::result::diagnostic_result;
use model::{DeliverInput, PlanInput, RunbookInput};
use pi_session::ToolDefinition;
use sequence_store::DirectoryLock;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use storage::{Storage, cancelled};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct RunbookStore {
    storage: Storage,
    gate: Arc<crate::update::activity::ActivityGate>,
}
impl RunbookStore {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, String> {
        Ok(Self {
            storage: Storage::new(root.as_ref())?,
            gate: crate::update::activity::ActivityGate::for_root(root.as_ref())?,
        })
    }
    async fn locked<T, F>(
        &self,
        relative: PathBuf,
        cancel: CancellationToken,
        task: F,
    ) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&Storage, &Path, &CancellationToken) -> Result<T, String> + Send + 'static,
    {
        let activity = self.gate.enter()?;
        static WRITERS: OnceLock<Arc<Mutex<()>>> = OnceLock::new();
        let writers = WRITERS.get_or_init(|| Arc::new(Mutex::new(()))).clone();
        let child = cancel.child_token();
        let _cancel_on_drop = child.clone().drop_guard();
        let permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err("命令交付已取消。".into()),
            permit = writers.lock_owned() => permit,
        };
        let storage = self.storage.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _activity = activity;
            cancelled(&child)?;
            let _lease = storage.pin_directory(&relative, true)?;
            let dir = storage.directory(&relative, false)?;
            let _lock = DirectoryLock::acquire(&dir)?;
            cancelled(&child)?;
            task(&storage, &dir, &child)
        })
        .await
        .map_err(|_| "命令交付工作线程异常退出。".to_string())?
    }
}

pub fn definitions(root: &Path) -> Result<Vec<ToolDefinition>, String> {
    let store = RunbookStore::new(root)?;
    let plan_store = store.clone();
    let mut plan = ToolDefinition::from_typed(
        "command_plan",
        "Command Plan",
        "保存或修订手动修复方案，返回完整命令、稳定 ID 与版本。只写工具包 agent/home/command-plans，不执行命令。",
        move |_, input: PlanInput, cancel, _, _| {
            let store = plan_store.clone();
            async move { diagnostic_result(store.plan(input, cancel.unwrap_or_default()).await?) }
        },
    );
    let deliver_store = store.clone();
    let mut deliver = ToolDefinition::from_typed(
        "runbook_deliver",
        "Deliver Runbook",
        "按已核对的 runbookId 与最新 version 导出 UTF-8 BOM + CRLF 文本。同版本重试沿用原文件，现场修改过的文件不覆盖。",
        move |_, input: DeliverInput, cancel, _, _| {
            let store = deliver_store.clone();
            async move { diagnostic_result(store.deliver(input, cancel.unwrap_or_default()).await?) }
        },
    );
    let mut runbook = ToolDefinition::from_typed(
        "runbook",
        "Command Runbook",
        "单步交付手动修复清单至 artifacts/<日期>/runbook。序号当天递增；传入已有 sequence 修正原清单，只写 TXT，不执行命令。",
        move |_, input: RunbookInput, cancel, _, _| {
            let store = store.clone();
            async move { diagnostic_result(store.write(input, cancel.unwrap_or_default()).await?) }
        },
    );
    for (definition, snippet) in [
        (
            &mut plan,
            "Prepare or revise a versioned manual repair command plan",
        ),
        (
            &mut deliver,
            "Export an agreed command plan version as a copy-paste text file",
        ),
        (
            &mut runbook,
            "Write or revise a sequence-based manual repair text file",
        ),
    ] {
        definition.prompt_snippet = Some(snippet.into());
        definition.prompt_guidelines = Some(vec![
            "Read skills/runbook/SKILL.md. Merge commands of the same risk level. Do not include diagnostic data collection, destructive or system-breaking operations. No tool executes commands.".into(),
            "Use command_plan for every new or revised command batch. Revisions require the exact runbookId and baseVersion plus the full items. On conflict, inspect the latest saved plan first.".into(),
            "Explain the plan and obtain the member's agreement before runbook_deliver. Use runbook only for an explicit immediate TXT request or an existing sequence-based file. Repeat the returned file path and manual execution requirements.".into(),
        ]);
    }
    Ok(vec![plan, deliver, runbook])
}

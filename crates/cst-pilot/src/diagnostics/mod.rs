pub mod disk;
pub mod driver;
pub mod eventlog;
pub mod hardware;
pub mod ls;
pub mod native_job;
pub mod result;
pub mod scan;
pub mod startup;
mod storage_tools;
pub mod sys;
pub mod sys_load;
pub mod volume;
pub mod walk;
pub mod win_native;
pub mod wz_index;

use pi_session::ToolDefinition;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use win_native::{Control, DiagnosticSource, WindowsSource};

pub fn definitions(root: &std::path::Path) -> Vec<ToolDefinition> {
    let source: Arc<dyn DiagnosticSource> = Arc::new(WindowsSource);
    let mut tools = definitions_with(source.clone());
    tools.retain(|tool| tool.name != "disk" && tool.name != "sys");
    tools.push(sys::definition(root, source.clone()));
    tools.extend(storage_tools::definitions(root, source));
    tools
}
pub fn definitions_with(source: Arc<dyn DiagnosticSource>) -> Vec<ToolDefinition> {
    vec![
        definition(
            "sys",
            "整机负载与进程 CPU / 内存排行",
            source.clone(),
            sys::run,
        ),
        definition(
            "disk",
            "卷空间、物理盘与可靠性计数器",
            source.clone(),
            disk::run,
        ),
        definition("driver", "设备状态与驱动信息", source.clone(), driver::run),
        definition(
            "startup",
            "注册表、启动文件夹与自启服务",
            source.clone(),
            startup::run,
        ),
        definition(
            "eventlog",
            "Windows 事件日志场景查询与详情",
            source,
            eventlog::run,
        ),
    ]
}
fn definition<P: DeserializeOwned + JsonSchema + Send + 'static>(
    name: &str,
    description: &str,
    source: Arc<dyn DiagnosticSource>,
    run: fn(&dyn DiagnosticSource, P, &Control) -> Result<Value, String>,
) -> ToolDefinition {
    let mut definition =
        ToolDefinition::from_typed(name, name, description, move |_, input: P, cancel, _, _| {
            let source = source.clone();
            async move {
                let value = execute(
                    source,
                    input,
                    cancel.unwrap_or_default(),
                    Duration::from_secs(30),
                    run,
                )
                .await?;
                result::diagnostic_result(value)
            }
        });
    definition.prompt_snippet = Some(format!(
        "只读查询{description}。结果为 JSON，单位与缺失说明见 notice。"
    ));
    definition
}

/// Permits stay with the OS worker even when the caller times out. Repeated hung
/// native calls cannot exhaust Tokio's blocking pool or create unbounded workers.
pub async fn execute<P: Send + 'static, F>(
    source: Arc<dyn DiagnosticSource>,
    input: P,
    cancel: CancellationToken,
    timeout: Duration,
    run: F,
) -> Result<Value, String>
where
    F: FnOnce(&dyn DiagnosticSource, P, &Control) -> Result<Value, String> + Send + 'static,
{
    static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let workers = WORKERS.get_or_init(|| Arc::new(Semaphore::new(4))).clone();
    let control = Control::new(cancel.child_token(), timeout);
    let _cancel_on_drop = control.cancel.clone().drop_guard();
    let operation = async {
        let permit = workers
            .acquire_owned()
            .await
            .map_err(|_| "诊断工作线程已关闭。")?;
        let worker = control.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            worker.check()?;
            let value = run(source.as_ref(), input, &worker)?;
            worker.check()?;
            Ok(value)
        })
        .await
        .map_err(|_| "诊断工作线程异常退出。")?
    };
    tokio::select! {
        biased;
        _=cancel.cancelled()=>Err("诊断已取消。".into()),
        _=tokio::time::sleep(timeout)=>{control.cancel.cancel();Err("诊断超时。".into())},
        result=operation=>result,
    }
}

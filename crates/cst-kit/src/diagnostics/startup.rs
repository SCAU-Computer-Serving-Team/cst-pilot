use super::win_native::{CIMV2, Control, DiagnosticSource, add_errors, collect, selected};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StartupInput {}

pub fn approval_disabled(is_run_once: bool, bytes: Option<&[u8]>) -> Option<bool> {
    if is_run_once {
        None
    } else {
        bytes.and_then(|b| b.first()).map(|b| b % 2 == 1)
    }
}
pub fn run(
    source: &dyn DiagnosticSource,
    _: StartupInput,
    control: &Control,
) -> Result<Value, String> {
    let snapshot = source.startup(control).map_err(|e| e.to_string())?;
    let mut errors = snapshot.errors;
    let mut services = collect(
        source,
        CIMV2,
        "SELECT Name,DisplayName,State,PathName FROM Win32_Service WHERE StartMode='Auto'",
        control,
        &mut errors,
    );
    services.sort_by(|a, b| {
        (b["State"] == "Running")
            .cmp(&(a["State"] == "Running"))
            .then_with(|| a["Name"].as_str().cmp(&b["Name"].as_str()))
    });
    let mut result = json!({"regItems":snapshot.reg_items,"startupFolders":snapshot.folders,"services":services.iter().map(|r|{
        let mut row=selected(r,&[("name","Name"),("display","DisplayName"),("state","State")]);
        row["path"]=json!(r["PathName"].as_str().map(|s|if s.chars().count()>140{s.chars().take(140).collect::<String>()+"..."}else{s.into()}));row
    }).collect::<Vec<_>>(),"notice":"只读盘点当前运行系统的注册表 Run/RunOnce、启动文件夹与自动启动服务。disabled 为 true/false/null 三态；RunOnce 不套用 Run 禁用记录。未采集计划任务或 WMI 事件订阅。"});
    control.check()?;
    add_errors(&mut result, errors);
    Ok(json!({"startup":result}))
}

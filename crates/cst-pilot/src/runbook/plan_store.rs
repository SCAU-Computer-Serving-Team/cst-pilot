use chrono::{DateTime, Local, NaiveDateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::RunbookStore;
use super::model::{DeliverInput, Plan, PlanInput, artifact_date, build_body, validate};
use super::storage::{Storage, cancelled, io_error};

const MAX_VERSION: u64 = 9_007_199_254_740_991;
impl RunbookStore {
    pub async fn plan(&self, input: PlanInput, cancel: CancellationToken) -> Result<Value, String> {
        let (title, commands) = validate(&input.title, &input.items)?;
        if input.runbook_id.is_some() != input.base_version.is_some() {
            return Err("修订方案须同时提供 runbookId 与 baseVersion。".into());
        }
        if let Some(id) = input.runbook_id.as_deref() {
            valid_id(id)?;
        }
        if input
            .base_version
            .is_some_and(|v| v == 0 || v >= MAX_VERSION)
        {
            return Err("baseVersion 必须是可递增的正整数。".into());
        }
        if let Some(day) = input.date.as_deref() {
            artifact_date(Some(day))?;
        }
        self.locked(PathBuf::from("agent/home/command-plans"), cancel, move |storage, _, cancel| {
            let previous = input.runbook_id.as_deref().map(|id| latest(storage, id, cancel)).transpose()?;
            if let Some(plan) = &previous
                && Some(plan.version) != input.base_version {
                return Err(format!("方案已有新版本 {}，请核对最新命令后再修订。", plan.version));
            }
            let day = artifact_date(input.date.as_deref().or_else(|| previous.as_ref().map(|p| p.day.as_str())))?;
            if previous.as_ref().is_some_and(|p| p.day != day) {
                return Err("已有方案的归档日期不能修改。".into());
            }
            let now = Local::now();
            let plan = Plan {
                runbook_id: previous.as_ref().map(|p| p.runbook_id.clone()).unwrap_or_else(|| Uuid::new_v4().to_string()),
                version: previous.as_ref().map_or(1, |p| p.version + 1),
                day, created_at: now.with_timezone(&Utc).to_rfc3339_opts(SecondsFormat::Millis, true),
                time_text: now.format("%Y-%m-%d %H:%M").to_string(),
                title, level: input.level, level_label: input.level.label().into(), commands,
            };
            let relative = PathBuf::from("agent/home/command-plans").join(&plan.runbook_id);
            let _lease = storage.pin_directory(&relative, true)?;
            let dir = storage.directory(&relative, false)?;
            let bytes = serde_json::to_vec(&plan).map_err(|_| "方案无法编码。")?;
            storage.atomic(&dir.join(format!("v{}.json", plan.version)), &bytes, false, cancel)?;
            let mut value = plan_value(&plan)?;
            value["action"] = json!("planned");
            value["notice"] = json!("命令方案已保存，尚未导出文本。核对后用 runbook_deliver 交付此 ID 和版本；命令需手动执行。");
            Ok(json!({"runbook":value}))
        }).await
    }
    pub async fn deliver(
        &self,
        input: DeliverInput,
        cancel: CancellationToken,
    ) -> Result<Value, String> {
        valid_id(&input.runbook_id)?;
        if input.version == 0 || input.version > MAX_VERSION {
            return Err("version 必须是正整数。".into());
        }
        self.locked(
            PathBuf::from("agent/home/command-plans"),
            cancel,
            move |storage, _, cancel| {
                let plan = latest(storage, &input.runbook_id, cancel)?;
                if plan.version != input.version {
                    return Err(format!(
                        "方案已有新版本 {}，请核对后交付最新版本。",
                        plan.version
                    ));
                }
                let relative = PathBuf::from("artifacts")
                    .join(&plan.day)
                    .join("runbook")
                    .join(&plan.runbook_id);
                let _lease = storage.pin_directory(&relative, true)?;
                let dir = storage.directory(&relative, false)?;
                let name = format!("v{}-{}-{}.txt", plan.version, plan.level_label, plan.title);
                let file = dir.join(&name);
                let body = build_body(&plan.title, plan.level, &plan.commands, &plan.time_text);
                match storage.read(&file)? {
                    Some(existing) if existing != body => {
                        return Err("已交付文件被修改，请核对后修订方案，避免覆盖现场内容。".into());
                    }
                    Some(_) => (),
                    None => storage.atomic(&file, &body, false, cancel)?,
                }
                let mut value = plan_value(&plan)?;
                for (key, field) in [
                    ("action", json!("delivered")),
                    ("file", json!(file)),
                    ("dir", json!(dir)),
                    ("name", json!(name)),
                    ("bytes", json!(body.len())),
                    ("encoding", json!("utf-8-bom")),
                ] {
                    value[key] = field;
                }
                Ok(json!({"runbook":value}))
            },
        )
        .await
    }
}
fn valid_id(id: &str) -> Result<(), String> {
    if !Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id) {
        return Err("清单 ID 无效。".into());
    }
    Ok(())
}
fn latest(storage: &Storage, id: &str, cancel: &CancellationToken) -> Result<Plan, String> {
    valid_id(id)?;
    let modern = PathBuf::from("agent/home/command-plans").join(id);
    let relative = match fs::symlink_metadata(storage.root.join(&modern)) {
        Ok(_) => modern,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            PathBuf::from("outbox/.plans").join(id)
        }
        Err(e) => return Err(io_error("读取方案目录", e)),
    };
    let _lease = storage.pin_directory(&relative, false)?;
    let dir = storage
        .directory(&relative, false)
        .map_err(|_| "命令方案不存在或目录包含链接，请检查后重试。")?;
    let mut version = 0;
    for entry in fs::read_dir(&dir).map_err(|e| io_error("读取方案目录", e))? {
        cancelled(cancel)?;
        let name = entry
            .map_err(|e| io_error("读取方案文件名", e))?
            .file_name();
        let name = name.to_string_lossy();
        if let Some(number) = name.strip_prefix('v').and_then(|s| s.strip_suffix(".json"))
            && !number.is_empty()
            && !number.starts_with('0')
            && number.bytes().all(|c| c.is_ascii_digit())
        {
            let number = number
                .parse::<u64>()
                .ok()
                .filter(|v| *v <= MAX_VERSION)
                .ok_or("命令方案版本号损坏，请检查方案目录。")?;
            version = version.max(number);
        }
    }
    if version == 0 {
        return Err("命令方案尚未保存，请重新生成。".into());
    }
    let raw = storage
        .read(&dir.join(format!("v{version}.json")))?
        .ok_or("命令方案文件缺失。")?;
    let mut plan: Plan = serde_json::from_slice(&raw)
        .map_err(|_| "命令方案记录损坏，请检查或重新生成，原文件已保留。")?;
    if plan.runbook_id != id
        || plan.version != version
        || artifact_date(Some(&plan.day)).is_err()
        || DateTime::parse_from_rfc3339(&plan.created_at).is_err()
        || !NaiveDateTime::parse_from_str(&plan.time_text, "%Y-%m-%d %H:%M")
            .is_ok_and(|time| time.format("%Y-%m-%d %H:%M").to_string() == plan.time_text)
    {
        return Err("命令方案记录损坏，请检查或重新生成，原文件已保留。".into());
    }
    let (title, commands) =
        validate(&plan.title, &plan.commands).map_err(|_| "命令方案内容损坏，原文件已保留。")?;
    plan.title = title;
    plan.commands = commands;
    plan.level_label = plan.level.label().into();
    Ok(plan)
}
fn plan_value(plan: &Plan) -> Result<Value, String> {
    let mut value = serde_json::to_value(plan).map_err(|_| "方案结果无法编码。")?;
    value["items"] = json!(plan.commands.len());
    Ok(value)
}

use chrono::{DateTime, Local, NaiveDateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::RunbookStore;
use super::model::{
    DeliverInput, Plan, PlanInput, RunbookAction, RunbookToolInput, artifact_date, build_body,
    validate,
};
use super::storage::{Storage, StorageAtomicExt, cancelled, io_error};

const MAX_VERSION: u64 = 9_007_199_254_740_991;
const MAX_SEQUENCE: u16 = 999;

impl RunbookStore {
    pub async fn execute(
        &self,
        input: RunbookToolInput,
        cancel: CancellationToken,
    ) -> Result<Value, String> {
        match input.action {
            RunbookAction::Plan => {
                if input.version.is_some() {
                    return Err("action=plan 时修订请使用 baseVersion 而非 version。".into());
                }
                let title = input.title.ok_or("action=plan 必须提供 title。")?;
                let level = input.level.ok_or("action=plan 必须提供 level。")?;
                let items = input.items.ok_or("action=plan 必须提供 items。")?;
                self.plan(
                    PlanInput {
                        title,
                        level,
                        items,
                        date: input.date,
                        runbook_id: input.runbook_id,
                        base_version: input.base_version,
                        export_txt: input.export_txt,
                    },
                    cancel,
                )
                .await
            }
            RunbookAction::Export => {
                if input.title.is_some()
                    || input.level.is_some()
                    || input.items.is_some()
                    || input.date.is_some()
                    || input.base_version.is_some()
                    || input.export_txt
                {
                    return Err(
                        "action=export 仅接受 runbookId 与 version，不能同时传方案编辑字段。"
                            .into(),
                    );
                }
                let runbook_id = input
                    .runbook_id
                    .ok_or("action=export 必须提供 runbookId。")?;
                let version = input.version.ok_or("action=export 必须提供 version。")?;
                self.deliver(
                    DeliverInput {
                        runbook_id,
                        version,
                    },
                    cancel,
                )
                .await
            }
        }
    }

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
        let export_txt = input.export_txt;
        self.locked_plans(cancel, move |plan_storage, plans_rel, root_storage, cancel| {
            let previous = input
                .runbook_id
                .as_deref()
                .map(|id| latest(plan_storage, plans_rel, id, cancel))
                .transpose()?;
            if let Some(plan) = &previous
                && Some(plan.version) != input.base_version
            {
                return Err(format!(
                    "方案已有新版本 {}，请核对最新命令后再修订。",
                    plan.version
                ));
            }
            let day = artifact_date(
                input
                    .date
                    .as_deref()
                    .or_else(|| previous.as_ref().map(|p| p.day.as_str())),
            )?;
            if previous.as_ref().is_some_and(|p| p.day != day) {
                return Err("已有方案的归档日期不能修改。".into());
            }
            let sequence = match &previous {
                Some(prev) if prev.sequence.is_some_and(|s| s >= 1) => prev.sequence.unwrap(),
                _ => next_sequence_for_day(plan_storage, plans_rel, root_storage, &day, cancel)?,
            };
            let now = Local::now();
            let plan = Plan {
                runbook_id: previous
                    .as_ref()
                    .map(|p| p.runbook_id.clone())
                    .unwrap_or_else(|| Uuid::new_v4().to_string()),
                sequence: Some(sequence),
                version: previous.as_ref().map_or(1, |p| p.version + 1),
                day,
                created_at: now
                    .with_timezone(&Utc)
                    .to_rfc3339_opts(SecondsFormat::Millis, true),
                time_text: now.format("%Y-%m-%d %H:%M").to_string(),
                title,
                level: input.level,
                level_label: input.level.label().into(),
                commands,
            };
            let relative = plans_rel.join(&plan.runbook_id);
            let _lease = plan_storage.pin_directory(&relative, true)?;
            let dir = plan_storage.directory(&relative, false)?;
            let bytes = serde_json::to_vec(&plan).map_err(|_| "方案无法编码。")?;
            plan_storage.atomic(
                &dir.join(format!("v{}.json", plan.version)),
                &bytes,
                false,
                cancel,
            )?;
            if export_txt {
                return write_exported_txt(&plan, root_storage, cancel).map_err(|err| {
                    format!(
                        "命令方案已保存（runbookId={}, version={}），但 TXT 导出失败：{err}",
                        plan.runbook_id, plan.version
                    )
                });
            }
            let mut value = plan_value(&plan)?;
            value["action"] = json!("planned");
            value["notice"] = json!(
                "命令方案已保存，尚未导出文本。队员可在卡片一键导出并用记事本打开 TXT，或通过 runbook(action=\"export\") 导出；命令需手动执行。"
            );
            Ok(json!({"runbook":value}))
        })
        .await
    }

    pub async fn deliver(
        &self,
        input: DeliverInput,
        cancel: CancellationToken,
    ) -> Result<Value, String> {
        self.export_plan(input.runbook_id, input.version, true, cancel)
            .await
    }

    pub async fn export_version(
        &self,
        runbook_id: String,
        version: u64,
        cancel: CancellationToken,
    ) -> Result<Value, String> {
        self.export_plan(runbook_id, version, false, cancel).await
    }

    async fn export_plan(
        &self,
        runbook_id: String,
        version: u64,
        require_latest: bool,
        cancel: CancellationToken,
    ) -> Result<Value, String> {
        valid_id(&runbook_id)?;
        if version == 0 || version > MAX_VERSION {
            return Err("version 必须是正整数。".into());
        }
        self.locked_plans(
            cancel,
            move |plan_storage, plans_rel, root_storage, cancel| {
                let mut plan = if require_latest {
                    let latest_plan = latest(plan_storage, plans_rel, &runbook_id, cancel)?;
                    if latest_plan.version != version {
                        return Err(format!(
                            "方案已有新版本 {}，请核对后导出最新版本。",
                            latest_plan.version
                        ));
                    }
                    latest_plan
                } else {
                    load_version(plan_storage, plans_rel, &runbook_id, version)?
                };
                if plan.sequence.is_none() {
                    let body = build_body(&plan.title, plan.level, &plan.commands, &plan.time_text);
                    let existing_seq = find_existing_exported_sequence(
                        root_storage,
                        &plan.day,
                        &plan.level_label,
                        &plan.title,
                        plan.version,
                        &body,
                        cancel,
                    )?;
                    plan.sequence = Some(match existing_seq {
                        Some(seq) => seq,
                        None => next_sequence_for_day(
                            plan_storage,
                            plans_rel,
                            root_storage,
                            &plan.day,
                            cancel,
                        )?,
                    });
                }
                write_exported_txt(&plan, root_storage, cancel)
            },
        )
        .await
    }
}

fn write_exported_txt(
    plan: &Plan,
    root_storage: &Storage,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let relative = PathBuf::from("artifacts").join(&plan.day).join("runbook");
    let _lease = root_storage.pin_directory(&relative, true)?;
    let dir = root_storage.directory(&relative, false)?;
    let seq = plan.sequence.unwrap_or(1).max(1);
    let name = format!(
        "{seq:02}-{}-{}-v{}.txt",
        plan.level_label, plan.title, plan.version
    );
    let file = dir.join(&name);
    let body = build_body(&plan.title, plan.level, &plan.commands, &plan.time_text);
    match root_storage.read(&file)? {
        Some(existing) if existing != body => {
            return Err("已导出文件被修改，请核对后修订方案，避免覆盖现场内容。".into());
        }
        Some(_) => (),
        None => root_storage.atomic(&file, &body, false, cancel)?,
    }
    let mut value = plan_value(plan)?;
    for (key, field) in [
        ("action", json!("exported")),
        ("file", json!(file)),
        ("dir", json!(dir)),
        ("name", json!(name)),
        ("bytes", json!(body.len())),
        ("encoding", json!("utf-8-bom")),
        (
            "notice",
            json!(
                "命令清单已导出至 artifacts/<日期>/runbook，文件是纯文本且不会自动执行；请原样转述 file 路径，队员打开后逐条复制命令。"
            ),
        ),
    ] {
        value[key] = field;
    }
    Ok(json!({"runbook":value}))
}

fn next_sequence_for_day(
    plan_storage: &Storage,
    plans_rel: &Path,
    root_storage: &Storage,
    day: &str,
    cancel: &CancellationToken,
) -> Result<u16, String> {
    let mut max_seq: u16 = 0;

    // 1. Scan saved plans in `data/plans/<uuid>/v*.json` matching `day`.
    if let Ok(plans_dir) = plan_storage.directory(plans_rel, false)
        && let Ok(entries) = fs::read_dir(&plans_dir)
    {
        for entry in entries.flatten() {
            cancelled(cancel)?;
            let name = entry.file_name();
            let id = name.to_string_lossy();
            if valid_id(&id).is_err() {
                continue;
            }
            if let Ok(plan) = latest(plan_storage, plans_rel, &id, cancel)
                && plan.day == day
            {
                max_seq = max_seq.max(plan.sequence.unwrap_or(0));
            }
        }
    }

    // 2. Also scan existing TXT files in `artifacts/<day>/runbook/` to avoid sequence collision.
    let art_rel = PathBuf::from("artifacts").join(day).join("runbook");
    if let Ok(_lease) = root_storage.pin_directory(&art_rel, false)
        && let Ok(art_dir) = root_storage.directory(&art_rel, false)
        && let Ok(entries) = fs::read_dir(&art_dir)
    {
        for entry in entries.flatten() {
            cancelled(cancel)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(seq) = parse_filename_sequence(&name) {
                max_seq = max_seq.max(seq);
            }
        }
    }

    let next = max_seq.saturating_add(1);
    if next > MAX_SEQUENCE {
        return Err("当天清单序号已达到 999 上限，请在次日生成新清单。".into());
    }
    Ok(next)
}

fn find_existing_exported_sequence(
    root_storage: &Storage,
    day: &str,
    level_label: &str,
    title: &str,
    version: u64,
    expected_body: &[u8],
    cancel: &CancellationToken,
) -> Result<Option<u16>, String> {
    let art_rel = PathBuf::from("artifacts").join(day).join("runbook");
    let Ok(_lease) = root_storage.pin_directory(&art_rel, false) else {
        return Ok(None);
    };
    let Ok(art_dir) = root_storage.directory(&art_rel, false) else {
        return Ok(None);
    };
    let suffix = format!("-{level_label}-{title}-v{version}.txt");
    if let Ok(entries) = fs::read_dir(&art_dir) {
        for entry in entries.flatten() {
            cancelled(cancel)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(num) = name.strip_suffix(&suffix)
                && (2..=3).contains(&num.len())
                && num.bytes().all(|b| b.is_ascii_digit())
                && let Ok(seq) = num.parse::<u16>()
                && seq >= 1
                && let Ok(Some(bytes)) = root_storage.read(&art_dir.join(&name))
                && bytes == expected_body
            {
                return Ok(Some(seq));
            }
        }
    }
    Ok(None)
}

fn parse_filename_sequence(name: &str) -> Option<u16> {
    let (number, rest) = name.split_once('-')?;
    if !(2..=3).contains(&number.len()) || !number.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (level, title) = rest.split_once('-')?;
    if !matches!(level, "安全" | "较安全" | "需确认") || title.strip_suffix(".txt")?.is_empty()
    {
        return None;
    }
    number.parse::<u16>().ok().filter(|n| *n >= 1)
}

fn valid_id(id: &str) -> Result<(), String> {
    if !Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id) {
        return Err("清单 ID 无效。".into());
    }
    Ok(())
}

fn load_version(
    storage: &Storage,
    plans_rel: &Path,
    id: &str,
    version: u64,
) -> Result<Plan, String> {
    valid_id(id)?;
    let relative = plans_rel.join(id);
    let _lease = storage
        .pin_directory(&relative, false)
        .map_err(|_| "命令方案不存在或目录包含链接，请检查后重试。")?;
    let dir = storage
        .directory(&relative, false)
        .map_err(|_| "命令方案不存在或目录包含链接，请检查后重试。")?;
    let raw = storage
        .read(&dir.join(format!("v{version}.json")))?
        .ok_or("命令方案文件缺失。")?;
    parse_and_validate_plan(&raw, id, version)
}

fn latest(
    storage: &Storage,
    plans_rel: &Path,
    id: &str,
    cancel: &CancellationToken,
) -> Result<Plan, String> {
    valid_id(id)?;
    let relative = plans_rel.join(id);
    let _lease = storage
        .pin_directory(&relative, false)
        .map_err(|_| "命令方案不存在或目录包含链接，请检查后重试。")?;
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
    parse_and_validate_plan(&raw, id, version)
}

fn parse_and_validate_plan(raw: &[u8], id: &str, version: u64) -> Result<Plan, String> {
    let mut plan: Plan = serde_json::from_slice(raw)
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
    if plan.sequence == Some(0) {
        plan.sequence = None;
    }
    Ok(plan)
}

fn plan_value(plan: &Plan) -> Result<Value, String> {
    let mut value = serde_json::to_value(plan).map_err(|_| "方案结果无法编码。")?;
    value["items"] = json!(plan.commands.len());
    Ok(value)
}

use chrono::{Local, NaiveDate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Safe,
    Low,
    Confirm,
}
impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Self::Safe => "安全",
            Self::Low => "较安全",
            Self::Confirm => "需确认",
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Shell {
    Cmd,
    Powershell,
}
impl Shell {
    pub fn label(self) -> &'static str {
        match self {
            Self::Cmd => "cmd",
            Self::Powershell => "PowerShell",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RunbookItem {
    #[schemars(length(min = 1, max = 300))]
    pub summary: String,
    #[schemars(length(min = 1, max = 2000))]
    pub command: String,
    pub shell: Shell,
    #[serde(default)]
    pub admin: bool,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanInput {
    #[schemars(length(min = 1, max = 48))]
    pub title: String,
    pub level: Level,
    #[schemars(length(min = 1, max = 20))]
    pub items: Vec<RunbookItem>,
    pub date: Option<String>,
    pub runbook_id: Option<String>,
    pub base_version: Option<u64>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeliverInput {
    pub runbook_id: String,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991_u64))]
    pub version: u64,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunbookInput {
    #[schemars(length(min = 1, max = 48))]
    pub title: String,
    pub level: Level,
    #[schemars(length(min = 1, max = 20))]
    pub items: Vec<RunbookItem>,
    pub date: Option<String>,
    #[schemars(range(min = 1, max = 999))]
    pub sequence: Option<u16>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub runbook_id: String,
    pub version: u64,
    pub day: String,
    pub created_at: String,
    pub time_text: String,
    pub title: String,
    pub level: Level,
    #[serde(default)]
    pub level_label: String,
    pub commands: Vec<RunbookItem>,
}

pub fn artifact_date(value: Option<&str>) -> Result<String, String> {
    let day = value
        .map(str::to_owned)
        .unwrap_or_else(|| Local::now().format("%Y-%m-%d").to_string());
    if day.len() != 10
        || !day.as_bytes()[0].is_ascii_digit()
        || day.starts_with('0')
        || !NaiveDate::parse_from_str(&day, "%Y-%m-%d")
            .is_ok_and(|date| date.format("%Y-%m-%d").to_string() == day)
    {
        return Err("请求的产物日期无效，请使用 YYYY-MM-DD。".into());
    }
    Ok(day)
}

pub fn validate(title: &str, items: &[RunbookItem]) -> Result<(String, Vec<RunbookItem>), String> {
    let title = text(title, "title", 48)?;
    let title: String = title
        .chars()
        .filter(|c| !matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    // Windows ignores trailing dots/spaces in normal paths. Remove them before naming.
    let title = title.trim_end_matches(['.', ' ']);
    let title = if title.is_empty() {
        "命令清单"
    } else {
        title
    }
    .to_owned();
    if items.is_empty() || items.len() > 20 {
        return Err("命令数量必须是 1–20 条。".into());
    }
    let items = items
        .iter()
        .map(|item| {
            Ok(RunbookItem {
                summary: text(&item.summary, "说明", 300)?,
                command: text(&item.command, "命令", 2000)?,
                shell: item.shell,
                admin: item.admin,
            })
        })
        .collect::<Result<_, String>>()?;
    Ok((title, items))
}
fn text(value: &str, field: &str, max: usize) -> Result<String, String> {
    if value.chars().any(|c| {
        matches!(c, '\r' | '\n' | '\u{85}' | '\u{2028}' | '\u{2029}')
            || (c.is_control() && c != '\t')
    }) {
        return Err(format!("{field}不能包含换行或控制字符。"));
    }
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max {
        return Err(format!("{field}必须是 1–{max} 字的单行文本。"));
    }
    Ok(value.into())
}
pub fn build_body(title: &str, level: Level, items: &[RunbookItem], time: &str) -> Vec<u8> {
    let mut lines = vec![
        format!("{title}（{}） {time}", level.label()),
        String::new(),
    ];
    for item in items {
        let privilege = if item.admin {
            "管理员"
        } else {
            "普通权限"
        };
        lines.extend([
            format!("环境：{}（{privilege}）", item.shell.label()),
            format!("说明：{}", item.summary),
            "命令：".into(),
            item.command.clone(),
            String::new(),
        ]);
    }
    format!("\u{feff}{}", lines.join("\r\n")).into_bytes()
}

use super::win_native::{Control, DiagnosticSource, EventQuery, EventRecord, add_errors};
use regex::RegexBuilder;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

#[derive(Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum EventScope {
    #[default]
    Recent,
    Boot,
    Crash,
    Service,
    Disk,
    Security,
    Query,
    Detail,
}
#[derive(Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventInput {
    #[serde(default)]
    pub scope: EventScope,
    pub hours: Option<u32>,
    pub top: Option<usize>,
    pub level: Option<String>,
    pub kind: Option<String>,
    pub r#type: Option<String>,
    pub app: Option<String>,
    pub name: Option<String>,
    pub msg: Option<String>,
    pub log_name: Option<String>,
    pub ids: Option<Vec<u16>>,
    pub provider: Option<String>,
    pub record_id: Option<u64>,
    pub id: Option<u16>,
}
const BOOT: &[u16] = &[12, 13, 6005, 6006, 6009, 41, 6008, 1001, 1074, 19, 7045];
const SERVICE: &[u16] = &[
    7000, 7001, 7002, 7003, 7009, 7011, 7013, 7022, 7023, 7024, 7025, 7026, 7031, 7032, 7034, 7038,
    7041, 7043,
];
const DISK: &[u16] = &[7, 11, 50, 51, 55, 98, 129, 140, 153, 157];
const WHEA: &str = "Provider[@Name='Microsoft-Windows-WHEA-Logger']";

pub fn run(
    source: &dyn DiagnosticSource,
    input: EventInput,
    control: &Control,
) -> Result<Value, String> {
    let hours = input.hours.unwrap_or(24).clamp(1, 720);
    let top = input.top.unwrap_or(100).clamp(1, 100);
    if let Some(log) = &input.log_name {
        validate_log(log)?;
    }
    if input
        .level
        .as_deref()
        .is_some_and(|v| !matches!(v, "warn" | "error"))
    {
        return Err("level 只接受 warn 或 error。".into());
    }
    let mut errors = Vec::new();
    let admin = match source.admin() {
        Ok(v) => Some(v),
        Err(e) => {
            errors.push(e);
            None
        }
    };
    if matches!(input.scope, EventScope::Detail) {
        return detail(source, &input, control);
    }
    let (scope,logs,extra,filter)=match input.scope {
        EventScope::Recent=>("recent",vec!["System".into(),"Application".into()],level(input.level.as_deref().unwrap_or("warn")),None),
        EventScope::Boot=>{
            let clause=match input.kind.as_deref().unwrap_or("all"){
                "all"=>format!("({} or {WHEA})",ids(BOOT)),"unexpected"=>ids(&[41,6008]),"bluescreen"=>format!("(EventID=1001 or {WHEA})"),_=>return Err("kind 只接受 all、unexpected 或 bluescreen。".into())};
            ("boot",vec!["System".into()],clause,None)
        }
        EventScope::Crash=>("crash",vec!["Application".into()],"((Provider[@Name='Application Error'] and EventID=1000 and Level=2) or (Provider[@Name='.NET Runtime'] and EventID=1026 and Level=2) or (Provider[@Name='Windows Error Reporting'] and EventID=1001) or (Provider[@Name='Application Hang'] and EventID=1002) or (Provider[@Name='SideBySide'] and (EventID=33 or EventID=35)))".into(),input.app.as_deref()),
        EventScope::Service=>("service",vec!["System".into()],format!("(Provider[@Name='Service Control Manager'] and {})",ids(SERVICE)),input.name.as_deref()),
        EventScope::Disk=>("disk",vec!["System".into()],ids(DISK),None),
        EventScope::Security=>{
            let range=match input.r#type.as_deref().unwrap_or("all"){"all"=>ids(&[4624,4625,4740]),"logonFail"=>ids(&[4625]),"lockout"=>ids(&[4740]),_=>return Err("type 只接受 all、logonFail 或 lockout。".into())};
            ("security",vec!["Security".into()],range,None)
        }
        EventScope::Query=>{
            let mut clauses = Vec::new();
            if let Some(tier) = &input.level { clauses.push(level(tier)); }
            if let Some(values) = &input.ids && !values.is_empty() { clauses.push(ids(values)); }
            ("query",vec![input.log_name.clone().unwrap_or_else(||"System".into())],clauses.join(" and "),input.msg.as_deref())
        }
        EventScope::Detail=>unreachable!(),
    };
    if filter.is_some_and(|s| s.is_empty() || s.chars().count() > 200) {
        return Err("消息过滤词需为 1–200 个字符。".into());
    }
    let provider = if let Some(text) = &input.provider {
        if text.is_empty() || text.len() > 128 {
            return Err("provider 正则需为 1–128 字节。".into());
        }
        Some(
            RegexBuilder::new(text)
                .case_insensitive(true)
                .size_limit(64 * 1024)
                .dfa_size_limit(256 * 1024)
                .build()
                .map_err(|_| "provider 正则无效或过于复杂。")?,
        )
    } else {
        None
    };
    if scope == "security" && admin != Some(true) {
        let mut value = json!({"logs":logs,"hours":hours,"top":top,"total":0,"truncated":false,"events":[],"counts":[],"countsTruncated":false,"unreadable":0,"firstTime":null,"lastTime":null,"admin":admin,"degraded":true,"notice":"无法读取 Security 通道：需要管理员权限；当前结果不表示没有登录事件。"});
        add_errors(&mut value, errors);
        return Ok(json!({scope:value}));
    }
    let mut seen = HashSet::new();
    let mut events: Vec<EventRecord> = Vec::new();
    let mut counts: HashMap<(String, u16), (usize, String)> = HashMap::new();
    let mut total = 0;
    let mut unreadable = 0;
    let mut scan_truncated = false;
    let mut counts_truncated = false;
    let mut first: Option<String> = None;
    let mut last: Option<String> = None;
    let filter = filter.map(str::to_lowercase);
    for log in &logs {
        control.check()?;
        let time = format!(
            "TimeCreated[timediff(@SystemTime) <= {}]",
            u64::from(hours) * 3_600_000
        );
        let xpath = format!(
            "*[System[{time}{}]]",
            if extra.is_empty() {
                String::new()
            } else {
                format!(" and ({extra})")
            }
        );
        let mut visitor = |event: EventRecord| -> Result<(), String> {
            control.check()?;
            if !seen.insert((event.log.clone(), event.record_id)) {
                return Ok(());
            }
            if provider
                .as_ref()
                .is_some_and(|re| !re.is_match(&event.provider))
                || filter.as_ref().is_some_and(|text| {
                    !event
                        .message
                        .as_ref()
                        .is_some_and(|s| s.to_lowercase().contains(text))
                })
            {
                return Ok(());
            }
            total += 1;
            if first.as_ref().is_none_or(|t| event.time < *t) {
                first = Some(event.time.clone());
            }
            if last.as_ref().is_none_or(|t| event.time > *t) {
                last = Some(event.time.clone());
            }
            let key = (event.provider.clone(), event.id);
            if counts.contains_key(&key) || counts.len() < 100 {
                let entry = counts.entry(key).or_insert((0, event.time.clone()));
                entry.0 += 1;
                if event.time > entry.1 {
                    entry.1 = event.time.clone();
                }
            } else {
                counts_truncated = true;
            }
            events.push(event);
            events.sort_by(|a, b| {
                b.time
                    .cmp(&a.time)
                    .then_with(|| b.record_id.cmp(&a.record_id))
            });
            events.truncate(top);
            Ok(())
        };
        match source.events(
            &EventQuery {
                log: log.clone(),
                xpath,
                max_events: 100_000,
            },
            &mut visitor,
            control,
        ) {
            Ok(read) => {
                unreadable += read.unreadable;
                scan_truncated |= read.truncated;
            }
            Err(error) => errors.push(error),
        }
    }
    let mut count_rows:Vec<_>=counts.into_iter().map(|((provider,id),(n,last))|json!({"key":format!("{provider}/{id}"),"provider":provider,"id":id,"n":n,"last":last})).collect();
    count_rows.sort_by(|a, b| {
        b["n"]
            .as_u64()
            .cmp(&a["n"].as_u64())
            .then_with(|| a["key"].as_str().cmp(&b["key"].as_str()))
    });
    let mut value = json!({"logs":logs,"hours":hours,"top":top,"total":total,"truncated":total>top||scan_truncated,"events":events.iter().map(|e|record(e,200)).collect::<Vec<_>>(),"counts":count_rows,"countsTruncated":counts_truncated,"unreadable":unreadable,"firstTime":first,"lastTime":last,"admin":admin,"notice":"Windows Event Log 原生只读查询；列表消息最多 200 字符，counts 最多 100 组。unreadable 表示消息或记录无法读取，空结果需结合权限与采集错误判读。"});
    if total == 0 && errors.is_empty() && unreadable == 0 && !scan_truncated {
        value["noMatch"] = json!(true);
    }
    if scan_truncated {
        value["scanTruncated"] = json!(true);
        value["degraded"] = json!(true);
        value["notice"] = json!(
            "已达到单通道 100,000 条扫描上限；total 与 counts 为已扫描范围统计，请缩小时间窗。"
        );
    }
    if unreadable > 0 {
        value["degraded"] = json!(true);
    }
    add_errors(&mut value, errors);
    control.check()?;
    Ok(json!({scope:value}))
}

fn detail(
    source: &dyn DiagnosticSource,
    input: &EventInput,
    control: &Control,
) -> Result<Value, String> {
    let log = input.log_name.as_ref().ok_or("detail 必须提供 logName。")?;
    validate_log(log)?;
    let clause = match (input.record_id, input.id) {
        (Some(record), None) => format!("EventRecordID={record}"),
        (None, Some(id)) => format!("EventID={id}"),
        _ => return Err("detail 必须提供 recordId 或 id，且只提供一项。".into()),
    };
    let mut found = None;
    let read = source
        .events(
            &EventQuery {
                log: log.clone(),
                xpath: format!("*[System[{clause}]]"),
                max_events: 1,
            },
            &mut |event| {
                if found.is_none() {
                    found = Some(event);
                }
                Ok(())
            },
            control,
        )
        .map_err(|e| e.to_string())?;
    control.check()?;
    let mut value = found
        .as_ref()
        .map(|e| record(e, 20_000))
        .unwrap_or_else(|| json!({"logName":log}));
    value["found"] = json!(found.is_some());
    if read.unreadable > 0 {
        value["unreadable"] = json!(read.unreadable);
        value["degraded"] = json!(true);
    }
    if let Some(e) = &found {
        value["machine"] = json!(e.machine);
        if e.message.is_none() {
            value["degraded"] = json!(true);
        }
    }
    value["notice"] =
        json!("单条事件原文最多 20,000 字符，保留换行；消息资源不可读时 msg 为 null。");
    Ok(json!({"detail":value}))
}
fn ids(ids: &[u16]) -> String {
    format!(
        "({})",
        ids.iter()
            .map(|id| format!("EventID={id}"))
            .collect::<Vec<_>>()
            .join(" or ")
    )
}
fn level(tier: &str) -> String {
    if tier == "error" {
        "(Level=1 or Level=2)"
    } else {
        "(Level=1 or Level=2 or Level=3)"
    }
    .into()
}
fn validate_log(log: &str) -> Result<(), String> {
    if log.is_empty()
        || log.len() > 100
        || !log
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        || !log
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b" ./_-".contains(&b))
    {
        Err("logName 通道名无效。".into())
    } else {
        Ok(())
    }
}
fn record(e: &EventRecord, max: usize) -> Value {
    json!({"logName":e.log,"time":e.time,"recordId":e.record_id,"level":e.level,"levelName":match e.level{1=>"Critical",2=>"Error",3=>"Warning",4=>"Information",5=>"Verbose",_=>"LogAlways"},"provider":e.provider,"id":e.id,"msg":e.message.as_ref().map(|s|s.chars().take(max).collect::<String>())})
}

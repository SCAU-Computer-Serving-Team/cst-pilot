use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SpecialToolKind {
    SysOverview,
    Runbook,
    WebSearch,
    BitLocker,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionMeta {
    #[serde(default)]
    pub started_at: Option<u64>,
    #[serde(default)]
    pub completed_at: Option<u64>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub stage: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallInput {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: Map<String, Value>,
    #[serde(default)]
    pub execution: Option<ToolExecutionMeta>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ToolContentPart {
    Text {
        text: String,
    },
    Image {
        #[serde(rename = "mimeType")]
        mime_type: String,
        data: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultInput {
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub raw_text: String,
    #[serde(default)]
    pub images: Vec<(String, String)>,
    #[serde(default)]
    pub details: Option<Value>,
    #[serde(default)]
    pub is_error: bool,
}

impl ToolResultInput {
    pub fn from_message_json(msg: &Value) -> Self {
        let obj = msg.as_object();
        let mut raw_parts = Vec::new();
        let mut images = Vec::new();
        if let Some(content) = obj.and_then(|o| o.get("content")) {
            if let Some(s) = content.as_str() {
                raw_parts.push(s.to_owned());
            } else if let Some(arr) = content.as_array() {
                for part in arr {
                    match part.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(t) = part.get("text").and_then(Value::as_str) {
                                raw_parts.push(t.to_owned());
                            }
                        }
                        Some("image") => {
                            let mime = part
                                .get("mimeType")
                                .and_then(Value::as_str)
                                .unwrap_or("image/png")
                                .to_owned();
                            let data = part
                                .get("data")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned();
                            images.push((mime, data));
                        }
                        _ => {}
                    }
                }
            }
        }
        Self {
            tool_call_id: obj
                .and_then(|o| o.get("toolCallId"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            tool_name: obj
                .and_then(|o| o.get("toolName"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            raw_text: raw_parts.join("\n"),
            images,
            details: obj.and_then(|o| o.get("details")).cloned(),
            is_error: obj
                .and_then(|o| o.get("isError"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }
    }
}

/// Classifies the 4 canvas special persistent tool components.
pub fn special_tool_kind(call: &ToolCallInput) -> Option<SpecialToolKind> {
    let scope = call.arguments.get("scope").and_then(Value::as_str);
    if call.name == "sys" && scope.unwrap_or("overview") == "overview" {
        return Some(SpecialToolKind::SysOverview);
    }
    if matches!(
        call.name.as_str(),
        "runbook" | "command_plan" | "runbook_deliver"
    ) {
        return Some(SpecialToolKind::Runbook);
    }
    if call.name == "web_search" {
        return Some(SpecialToolKind::WebSearch);
    }
    if call.name == "bitlocker" && matches!(scope, Some("unlock" | "decrypt")) {
        return Some(SpecialToolKind::BitLocker);
    }
    None
}

pub fn is_standalone_tool(call: &ToolCallInput, result: Option<&ToolResultInput>) -> bool {
    special_tool_kind(call).is_some()
        || (call.name == "read" && result.is_some_and(|r| !r.images.is_empty()))
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolCallSegmentKind {
    Group,
    Standalone,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallSegment {
    pub kind: ToolCallSegmentKind,
    pub calls: Vec<ToolCallInput>,
}

pub fn split_tool_calls(
    calls: &[ToolCallInput],
    results: &BTreeMap<String, ToolResultInput>,
) -> Vec<ToolCallSegment> {
    let mut segments: Vec<ToolCallSegment> = Vec::new();
    for call in calls {
        let kind = if is_standalone_tool(call, results.get(&call.id)) {
            ToolCallSegmentKind::Standalone
        } else {
            ToolCallSegmentKind::Group
        };
        if kind == ToolCallSegmentKind::Group
            && let Some(last) = segments.last_mut()
            && last.kind == ToolCallSegmentKind::Group
        {
            last.calls.push(call.clone());
            continue;
        }
        segments.push(ToolCallSegment {
            kind,
            calls: vec![call.clone()],
        });
    }
    segments
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CardStatus {
    Success,
    Error,
    Degraded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolRunState {
    Pending,
    Running,
    Interrupted,
    Success,
    Error,
    Degraded,
}

pub fn compute_tool_state(
    result: Option<&ToolResultInput>,
    card_status: Option<CardStatus>,
    live: bool,
    started: bool,
) -> ToolRunState {
    let Some(res) = result else {
        return if live {
            if started {
                ToolRunState::Running
            } else {
                ToolRunState::Pending
            }
        } else {
            ToolRunState::Interrupted
        };
    };
    if res.is_error {
        ToolRunState::Error
    } else {
        match card_status.unwrap_or(CardStatus::Success) {
            CardStatus::Success => ToolRunState::Success,
            CardStatus::Error => ToolRunState::Error,
            CardStatus::Degraded => ToolRunState::Degraded,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoticeStatus {
    Default,
    Warning,
    Danger,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldRow {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListingItem {
    pub name: String,
    pub metrics: Vec<FieldRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandItem {
    pub shell: String,
    pub admin: bool,
    pub summary: String,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ToolBlock {
    Fields {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        rows: Vec<FieldRow>,
    },
    Listing {
        id: String,
        items: Vec<ListingItem>,
    },
    Text {
        id: String,
        text: String,
    },
    Image {
        id: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        data: String,
    },
    Notice {
        id: String,
        text: String,
        status: NoticeStatus,
    },
    Commands {
        id: String,
        items: Vec<CommandItem>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCardView {
    pub status: CardStatus,
    pub blocks: Vec<ToolBlock>,
    pub raw: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpecialStatus {
    Pending,
    Running,
    Interrupted,
    Success,
    Error,
    Degraded,
    Cancelled,
    Unknown,
}

impl From<ToolRunState> for SpecialStatus {
    fn from(value: ToolRunState) -> Self {
        match value {
            ToolRunState::Pending => Self::Pending,
            ToolRunState::Running => Self::Running,
            ToolRunState::Interrupted => Self::Interrupted,
            ToolRunState::Success => Self::Success,
            ToolRunState::Error => Self::Error,
            ToolRunState::Degraded => Self::Degraded,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunbookDeliveryView {
    pub name: String,
    pub file: String,
    pub level: String,
    pub count: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecialVolumeView {
    pub id: String,
    pub drive: String,
    pub conversion: String,
    pub conversion_code: Option<i64>,
    pub lock: String,
    pub percentage: Option<f64>,
    pub operation: String,
    pub reason: String,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SpecialToolView {
    SysOverview {
        status: SpecialStatus,
        title: String,
        summary: String,
        raw: String,
        fields: Vec<FieldRow>,
        errors: Vec<String>,
        machine: String,
        cpu: Option<f64>,
        cores: Option<i64>,
        memory: Option<f64>,
        capacity: String,
        uptime: String,
    },
    Runbook {
        status: SpecialStatus,
        title: String,
        summary: String,
        raw: String,
        fields: Vec<FieldRow>,
        errors: Vec<String>,
        commands: Vec<CommandItem>,
        level: String,
        count: Option<i64>,
        version: Option<i64>,
        planned: bool,
        delivery: Option<RunbookDeliveryView>,
    },
    WebSearch {
        status: SpecialStatus,
        title: String,
        summary: String,
        raw: String,
        fields: Vec<FieldRow>,
        errors: Vec<String>,
        queries: Vec<String>,
        successful: Option<i64>,
        unstructured: bool,
    },
    #[serde(rename = "bitlocker")]
    BitLocker {
        status: SpecialStatus,
        title: String,
        summary: String,
        raw: String,
        fields: Vec<FieldRow>,
        errors: Vec<String>,
        scope: String,
        volumes: Vec<SpecialVolumeView>,
    },
}

impl SpecialToolView {
    pub fn status(&self) -> SpecialStatus {
        match self {
            Self::SysOverview { status, .. }
            | Self::Runbook { status, .. }
            | Self::WebSearch { status, .. }
            | Self::BitLocker { status, .. } => *status,
        }
    }

    pub fn title(&self) -> &str {
        match self {
            Self::SysOverview { title, .. }
            | Self::Runbook { title, .. }
            | Self::WebSearch { title, .. }
            | Self::BitLocker { title, .. } => title.as_str(),
        }
    }

    pub fn summary(&self) -> &str {
        match self {
            Self::SysOverview { summary, .. }
            | Self::Runbook { summary, .. }
            | Self::WebSearch { summary, .. }
            | Self::BitLocker { summary, .. } => summary.as_str(),
        }
    }

    pub fn errors(&self) -> &[String] {
        match self {
            Self::SysOverview { errors, .. }
            | Self::Runbook { errors, .. }
            | Self::WebSearch { errors, .. }
            | Self::BitLocker { errors, .. } => errors.as_slice(),
        }
    }

    pub fn fields(&self) -> &[FieldRow] {
        match self {
            Self::SysOverview { fields, .. }
            | Self::Runbook { fields, .. }
            | Self::WebSearch { fields, .. }
            | Self::BitLocker { fields, .. } => fields.as_slice(),
        }
    }

    pub fn raw(&self) -> &str {
        match self {
            Self::SysOverview { raw, .. }
            | Self::Runbook { raw, .. }
            | Self::WebSearch { raw, .. }
            | Self::BitLocker { raw, .. } => raw.as_str(),
        }
    }
}

fn base_field_title(key: &str) -> Option<&'static str> {
    Some(match key {
        "count" => "数量",
        "total" => "总数",
        "top" => "返回条数",
        "hours" => "时间范围",
        "method" => "统计方式",
        "path" => "路径",
        "size" => "大小",
        "bytes" => "字节数",
        "pct" => "占比",
        "name" => "名称",
        "type" => "类型",
        "status" => "状态",
        "text" => "文本",
        "note" => "说明",
        "category" => "设备类",
        "trim" => "截断说明",
        "space" => "卷容量",
        "drive" | "label" => "卷标",
        "freeGB" => "剩余容量",
        "totalGB" => "总容量",
        "usedPct" => "已使用",
        "fileSystem" | "fs" => "文件系统",
        "driveType" => "类型",
        "physicalDisks" => "物理硬盘",
        "volumes" => "卷信息",
        "smart" => "SMART 状态",
        "FriendlyName" | "model" => "型号",
        "SerialNumber" => "序列号",
        "MediaType" => "介质",
        "BusType" | "bus" => "总线",
        "HealthStatus" => "健康",
        "OperationalStatus" | "state" => "运行状态",
        "DeviceId" | "deviceId" => "设备 ID",
        "sizeGB" => "容量",
        "root" => "扫描根",
        "filesScanned" => "已扫描文件",
        "elapsedSec" => "耗时",
        "budget" => "扫描上限",
        "topDirs" => "目录排行",
        "topFiles" => "单文件排行",
        "extAgg" => "扩展名汇总",
        "staleFiles" => "旧文件",
        "Wear" => "磨损",
        "PowerOnHours" => "通电时长",
        "ReadErrorsTotal" => "读取错误",
        "WriteErrorsTotal" => "写入错误",
        "ext" => "扩展名",
        "files" => "文件数",
        "cpuTotalPct" => "CPU 总占用",
        "logicalCores" => "逻辑核心",
        "mem" => "内存",
        "totalMB" => "总量",
        "usedMB" | "usedGB" => "已用",
        "freeMB" => "可用",
        "pagefile" => "页面文件",
        "allocMB" => "分配",
        "peakMB" => "峰值",
        "pool" => "内核池",
        "nonpagedMB" => "非分页",
        "pagedMB" => "分页",
        "machine" => "机型",
        "vendor" => "厂商",
        "cpu" => "处理器",
        "physicalCores" => "物理核心",
        "bios" => "BIOS",
        "biosDate" => "BIOS 日期",
        "totalProcs" => "进程总数",
        "cores" => "核心数",
        "intervalSec" => "采样间隔",
        "byCpu" | "cpuPct" => "CPU 占用",
        "byMem" => "内存占用",
        "wsMB" => "工作集",
        "pid" => "进程 ID",
        "disks" => "磁盘活动",
        "queueLen" => "队列长度",
        "busyPct" => "忙碌度",
        "readKBs" => "读取",
        "writeKBs" => "写入",
        "disk" => "磁盘",
        "byIo" => "进程 I/O",
        "ioKBs" => "读写速率",
        "byGpuPct" | "gpuPct" => "GPU 占用",
        "byDedicatedMB" => "显存占用",
        "dedicatedMB" => "显存",
        "engtypes" => "引擎",
        "adapters" => "显示适配器",
        "driver" | "drivers" => "驱动",
        "nvidia" => "NVIDIA 状态",
        "powerW" => "功耗",
        "vramUsedMB" => "显存已用",
        "vramTotalMB" => "显存总量",
        "utilPct" => "使用率",
        "sensors" => "传感器",
        "hw" => "硬件",
        "sensorCount" => "传感器数量",
        "thermalZones" | "zone" => "热区",
        "passivePct" => "被动散热",
        "tempC" => "温度",
        "frequency" => "频率",
        "avgPctOfMax" => "平均频率",
        "minPctOfMax" => "最低频率",
        "admin" => "管理员",
        "pawnio" => "PawnIO",
        "devices" => "设备",
        "class" => "设备类",
        "errorCode" => "错误码",
        "hardwareIds" => "硬件 ID",
        "connId" => "连接",
        "physical" => "物理设备",
        "connStatus" => "连接状态",
        "removable" => "可移动设备",
        "services" => "服务",
        "version" => "版本",
        "date" => "日期",
        "provider" => "提供程序",
        "net" => "网络",
        "bluetooth" => "蓝牙",
        "audio" => "音频",
        "events" => "事件",
        "time" => "时间",
        "logName" => "日志",
        "id" => "事件 ID",
        "levelName" => "级别",
        "recordId" => "记录号",
        "msg" => "简述",
        "counts" => "出现次数",
        "n" => "次数",
        "last" => "最近一次",
        "unreadable" => "不可读",
        "regItems" => "注册表自启",
        "startupFolders" => "启动文件夹",
        "source" | "sources" | "source_url" | "urls" => "来源",
        "command" => "命令",
        "disabled" => "状态",
        "scope" => "范围",
        "items" => "条目",
        "display" => "显示名",
        "collectionErrors" => "采集错误",
        "totalChildren" => "直接子项总数",
        "totalSize" => "总大小",
        "entries" => "子项大小",
        "unknownCount" => "未知大小",
        "omitted" => "省略项",
        "current" => "当前目录",
        "totalMatched" => "命中条数",
        "totalFiles" => "索引文件",
        "pageIndex" => "当前页",
        "hasMore" => "有下一页",
        "cursor" => "游标",
        "nextOffset" => "下一偏移",
        "file" => "文件",
        "sequence" | "rank" => "序号",
        "levelLabel" => "风险档",
        "encoding" => "文本格式",
        "dir" => "目录",
        "query" => "主张",
        "claims" => "主张判断",
        "confidence" => "可信度",
        "rationale" => "理由",
        "title" => "标题",
        "url" => "链接",
        "quality" => "质量",
        "fetched" => "取得正文",
        "passages" => "引文",
        "source_rank" => "来源序号",
        "sourceCount" | "urlCount" => "来源数",
        "passageCount" => "引文数",
        "responseId" => "研究结果标识",
        "successful" => "成功",
        "mode" => "模式",
        "totalChars" => "总字符",
        "hasImage" => "含图片",
        "resultCount" => "结果数",
        "contentLength" => "内容长度",
        "offset" => "当前偏移",
        "returnedChars" => "返回字符",
        "findMode" => "匹配模式",
        "matchCount" => "命中数",
        "returnedMatches" => "返回命中",
        "truncation" => "截断",
        "truncatedBy" => "截断方式",
        "totalLines" => "总行数",
        "outputLines" => "输出行数",
        "error" => "错误",
        _ => return None,
    })
}

fn resolve_label(tool_name: &str, scope: Option<&str>, key: &str) -> String {
    if tool_name == "get_search_content" {
        match key {
            "type" => return "结果类型".into(),
            "offset" => return "当前偏移".into(),
            "returnedChars" => return "返回字符".into(),
            "nextOffset" => return "下一偏移".into(),
            _ => {}
        }
    }
    if tool_name == "eventlog" && key == "machine" {
        return "机器".into();
    }
    if tool_name == "disk" && scope == Some("usage") && key == "totalGB" {
        return "总大小".into();
    }
    base_field_title(key)
        .map(str::to_owned)
        .unwrap_or_else(|| key.to_owned())
}

fn words(val: Option<&Value>) -> String {
    match val {
        None | Some(Value::Null) => "未知".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => {
            if let Some(f) = n.as_f64()
                && f.fract() == 0.0
                && f >= (i64::MIN as f64)
                && f <= (i64::MAX as f64)
            {
                format!("{}", f as i64)
            } else {
                n.to_string()
            }
        }
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn boolean_pair(key: &str) -> Option<(&'static str, &'static str)> {
    Some(match key {
        "disabled" => ("已禁用", "未禁用"),
        "fetched" => ("已取得正文", "未取得正文"),
        "hasMore" => ("有", "无"),
        "admin" | "physical" | "found" | "truncated" => ("是", "否"),
        _ => return None,
    })
}

fn measure(key: &str, val: Option<&Value>) -> String {
    if let Some((yes, no)) = boolean_pair(key) {
        return match val {
            None | Some(Value::Null) => "状态未知".into(),
            Some(Value::Bool(b)) => if *b { yes } else { no }.into(),
            Some(other) => if is_js_truthy(other) { yes } else { no }.into(),
        };
    }
    match val {
        None | Some(Value::Null) => "未知".into(),
        Some(Value::Bool(b)) => if *b { "是" } else { "否" }.into(),
        Some(other) => {
            let suffix = match key {
                "n" => " 次",
                "files" => " 个",
                "hours" => " 小时",
                "pct" => "%",
                "C" => " °C",
                "W" => " W",
                _ => [
                    ("Pct", "%"),
                    ("KBs", " KB/s"),
                    ("MBs", " MB/s"),
                    ("GBs", " GB/s"),
                    ("Sec", " 秒"),
                    ("MB", " MB"),
                    ("GB", " GB"),
                ]
                .iter()
                .find_map(|(end, suf)| key.ends_with(end).then_some(*suf))
                .unwrap_or(""),
            };
            format!("{}{suffix}", words(Some(other)))
        }
    }
}

fn is_js_truthy(val: &Value) -> bool {
    match val {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn is_ranking_key(key: &str) -> bool {
    matches!(
        key,
        "topDirs"
            | "topFiles"
            | "extAgg"
            | "staleFiles"
            | "byCpu"
            | "byMem"
            | "disks"
            | "byIo"
            | "byGpuPct"
            | "byDedicatedMB"
            | "counts"
            | "entries"
            | "sources"
    )
}

fn is_nullable_key(key: &str) -> bool {
    matches!(
        key,
        "smart"
            | "nvidia"
            | "lhmGpu"
            | "mem"
            | "pool"
            | "machine"
            | "uptime"
            | "frequency"
            | "stats"
            | "truncation"
            | "omitted"
            | "counts"
            | "events"
            | "sensors"
            | "devices"
            | "removable"
            | "entries"
    )
}

fn is_notice_key(key: &str) -> bool {
    matches!(
        key,
        "notice"
            | "infoNotice"
            | "smartNotice"
            | "degradedFrom"
            | "error"
            | "collectionErrors"
            | "counterErrors"
            | "smartErrors"
            | "degraded"
            | "outputTruncated"
            | "truncated"
            | "unreadable"
            | "truncation"
            | "omitted"
            | "stats"
    )
}

fn is_hidden_key(tool_name: &str, key: &str) -> bool {
    match tool_name {
        "eventlog" => matches!(key, "level" | "found" | "time" | "logName"),
        "disk" => key == "evt",
        "fetch_content" | "get_search_content" => key == "responseId",
        _ => false,
    }
}

const NAME_KEYS: &[&str] = &[
    "name",
    "path",
    "drive",
    "title",
    "provider",
    "disk",
    "ext",
    "hw",
    "zone",
    "scope",
    "FriendlyName",
    "display",
    "label",
];

fn is_diagnostic_tool(name: &str) -> bool {
    matches!(
        name,
        "disk" | "sys" | "driver" | "eventlog" | "startup" | "ls" | "bitlocker"
    )
}

fn is_text_tool(name: &str) -> bool {
    matches!(
        name,
        "read" | "ffgrep" | "fffind" | "fetch_content" | "get_search_content"
    )
}

fn first_non_empty(parts: &[Option<&Value>], fallback: &str) -> String {
    for part in parts.iter().flatten() {
        if !part.is_null() && part.as_str() != Some("") {
            return words(Some(part));
        }
    }
    fallback.to_owned()
}

fn group_heading(key: &str, entry: &Map<String, Value>, index: usize) -> Option<String> {
    let idx1 = (index + 1).to_string();
    Some(match key {
        "space" | "volumes" => format!("卷 {}", first_non_empty(&[entry.get("drive")], &idx1)),
        "physicalDisks" => format!("物理盘 · {}", index + 1),
        "smart" => format!("SMART 状态 · {}", index + 1),
        "devices" => format!("设备 · {}", index + 1),
        "removable" => format!("可移动设备 · {}", index + 1),
        "regItems" => format!(
            "注册表自启 · {}",
            first_non_empty(&[entry.get("name")], &idx1)
        ),
        "startupFolders" => format!(
            "启动文件夹 · {}",
            first_non_empty(&[entry.get("scope")], &idx1)
        ),
        "services" => format!(
            "自启服务 · {}",
            first_non_empty(&[entry.get("display"), entry.get("name")], &idx1)
        ),
        "events" => {
            let parts: Vec<String> = [entry.get("time"), entry.get("logName")]
                .into_iter()
                .flatten()
                .filter(|v| !v.is_null() && v.as_str() != Some(""))
                .map(|v| words(Some(v)))
                .collect();
            if parts.is_empty() {
                format!("事件 · {}", index + 1)
            } else {
                parts.join(" · ")
            }
        }
        "claims" => format!("主张判断 · {}", index + 1),
        "sensors" => format!("传感器 · {}", first_non_empty(&[entry.get("name")], &idx1)),
        "pagefile" => format!(
            "页面文件 · {}",
            first_non_empty(&[entry.get("name")], &idx1)
        ),
        "thermalZones" => format!("热区 · {}", first_non_empty(&[entry.get("zone")], &idx1)),
        "drivers" => format!("驱动 · {}", first_non_empty(&[entry.get("device")], &idx1)),
        _ => return None,
    })
}

/// Projects any normal open tool call + result into UI-agnostic `ToolCardView`,
/// matching `src/web/frontend/app/cards/map-tool.ts` across all 17 tools.
pub fn map_tool(call: &ToolCallInput, result: &ToolResultInput) -> ToolCardView {
    let raw = result.raw_text.clone();
    let detail = result.details.as_ref().and_then(Value::as_object);
    let scope: Option<String> = call
        .arguments
        .get("scope")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| match call.name.as_str() {
            "sys" => Some("overview".into()),
            "driver" => Some("problem".into()),
            "eventlog" => Some("recent".into()),
            _ => None,
        });
    let nested = if call.name == "bitlocker" {
        detail
            .and_then(|d| d.get("bitlocker"))
            .and_then(Value::as_object)
    } else if let (Some(sc), Some(d)) = (scope.as_deref(), detail) {
        d.get(sc).and_then(Value::as_object)
    } else {
        None
    };
    let source = if is_diagnostic_tool(&call.name) && nested.is_some() {
        nested
    } else {
        detail
    };

    let has_useful_data =
        source.is_some_and(|src| src.iter().any(|(k, v)| !is_notice_key(k) && !v.is_null()));
    let has_error = detail
        .and_then(|d| d.get("error"))
        .is_some_and(is_js_truthy)
        || source
            .and_then(|s| s.get("error"))
            .is_some_and(is_js_truthy);
    let partial_error = source.is_some_and(|src| {
        src.values().any(|v| {
            v.as_object()
                .and_then(|o| o.get("error"))
                .is_some_and(is_js_truthy)
        }) || [
            "degradedFrom",
            "collectionErrors",
            "smartErrors",
            "counterErrors",
        ]
        .iter()
        .any(|k| src.get(*k).is_some_and(is_js_truthy))
    });
    let partially_searched = detail
        .and_then(|d| {
            Some((
                d.get("successfulQueries")?.as_f64()?,
                d.get("queryCount")?.as_f64()?,
            ))
        })
        .is_some_and(|(succ, cnt)| succ < cnt);
    let partially_fetched = detail
        .and_then(|d| Some((d.get("successful")?.as_f64()?, d.get("urlCount")?.as_f64()?)))
        .is_some_and(|(succ, cnt)| succ < cnt);

    let status = if result.is_error || (has_error && !has_useful_data) {
        CardStatus::Error
    } else if has_error
        || partial_error
        || partially_searched
        || partially_fetched
        || source
            .and_then(|s| s.get("degraded"))
            .is_some_and(is_js_truthy)
        || detail
            .and_then(|d| d.get("degraded"))
            .is_some_and(is_js_truthy)
    {
        CardStatus::Degraded
    } else {
        CardStatus::Success
    };

    let mut blocks: Vec<ToolBlock> = Vec::new();
    let scope_ref = scope.as_deref();
    let label = |key: &str| resolve_label(&call.name, scope_ref, key);

    if status == CardStatus::Error {
        let err_val = detail
            .and_then(|d| d.get("error"))
            .filter(|v| !v.is_null())
            .or_else(|| source.and_then(|s| s.get("error")).filter(|v| !v.is_null()));
        let text = match err_val {
            Some(v) => words(Some(v)),
            None => raw.clone(),
        };
        blocks.push(ToolBlock::Notice {
            id: "error".into(),
            text,
            status: NoticeStatus::Danger,
        });
        return ToolCardView {
            status,
            blocks,
            raw,
        };
    }

    if call.name == "source_check" {
        let artifact = detail
            .and_then(|d| d.get("artifact"))
            .and_then(Value::as_object);
        if let Some(claims) = artifact
            .and_then(|a| a.get("claims"))
            .and_then(Value::as_array)
            && !claims.is_empty()
        {
            let mut rows = Vec::new();
            for (idx, item) in claims.iter().enumerate() {
                let claim = item.as_object();
                if idx == 0
                    && let Some(q) = artifact
                        .and_then(|a| a.get("query"))
                        .filter(|v| !v.is_null())
                {
                    rows.push(FieldRow {
                        label: "主张".into(),
                        value: words(Some(q)),
                    });
                }
                rows.push(FieldRow {
                    label: "状态".into(),
                    value: words(claim.and_then(|c| c.get("status"))),
                });
                rows.push(FieldRow {
                    label: "可信度".into(),
                    value: words(claim.and_then(|c| c.get("confidence"))),
                });
                rows.push(FieldRow {
                    label: "理由".into(),
                    value: words(claim.and_then(|c| c.get("rationale"))),
                });
            }
            blocks.push(ToolBlock::Fields {
                id: "claims".into(),
                title: Some("主张判断".into()),
                rows,
            });
        }
        if let Some(sources) = artifact
            .and_then(|a| a.get("sources"))
            .and_then(Value::as_array)
            && !sources.is_empty()
        {
            let mut m = Map::new();
            m.insert("sources".into(), Value::Array(sources.clone()));
            append_group(&call.name, scope_ref, &m, "sources", None, 0, &mut blocks);
        }
        if let Some(passages) = artifact
            .and_then(|a| a.get("passages"))
            .and_then(Value::as_array)
            && !passages.is_empty()
        {
            let mut m = Map::new();
            m.insert("passages".into(), Value::Array(passages.clone()));
            append_group(&call.name, scope_ref, &m, "passages", None, 0, &mut blocks);
        }
        if let Some(d) = detail {
            let rows: Vec<FieldRow> = ["responseId", "sourceCount", "passageCount"]
                .iter()
                .filter_map(|k| {
                    let v = d.get(*k)?;
                    if v.is_null() {
                        return None;
                    }
                    Some(FieldRow {
                        label: label(k),
                        value: measure(k, Some(v)),
                    })
                })
                .collect();
            blocks.push(ToolBlock::Fields {
                id: "source-metadata".into(),
                title: Some("继续查看".into()),
                rows,
            });
            push_notices(&call.name, scope_ref, d, &mut blocks);
        }
    } else if is_text_tool(&call.name) {
        if let Some(d) = detail {
            append_group(&call.name, scope_ref, d, "result", None, 0, &mut blocks);
        }
        if call.name == "read"
            && let Some(p) = call.arguments.get("path").filter(|v| is_js_truthy(v))
        {
            blocks.insert(
                0,
                ToolBlock::Fields {
                    id: "read-path".into(),
                    title: None,
                    rows: vec![FieldRow {
                        label: "绝对路径".into(),
                        value: words(Some(p)),
                    }],
                },
            );
        }
        if !raw.is_empty()
            && !(call.name == "read"
                && !result.images.is_empty()
                && raw.starts_with("Read image file ["))
        {
            blocks.push(ToolBlock::Text {
                id: "content".into(),
                text: raw.clone(),
            });
        }
        if call.name == "get_search_content"
            && detail
                .and_then(|d| d.get("truncated"))
                .is_some_and(is_js_truthy)
        {
            let next_off = words(detail.and_then(|d| d.get("nextOffset")));
            blocks.push(ToolBlock::Notice {
                id: "continue".into(),
                text: format!("JSON 字符片段，非完整结论 · 已截断，续读 offset={next_off}"),
                status: NoticeStatus::Warning,
            });
        }
        if let Some(d) = detail {
            push_notices(&call.name, scope_ref, d, &mut blocks);
        }
    } else if is_diagnostic_tool(&call.name) {
        if call.name == "startup"
            && let Some(startup) = detail
                .and_then(|d| d.get("startup"))
                .and_then(Value::as_object)
        {
            let summary_rows = [
                ("注册表自启项", "regItems"),
                ("启动文件夹", "startupFolders"),
                ("自启服务", "services"),
            ]
            .into_iter()
            .map(|(text, key)| FieldRow {
                label: text.into(),
                value: startup
                    .get(key)
                    .and_then(Value::as_array)
                    .map(|a| a.len().to_string())
                    .unwrap_or_else(|| "未知".into()),
            })
            .collect();
            blocks.push(ToolBlock::Fields {
                id: "startup-summary".into(),
                title: Some("自启概况".into()),
                rows: summary_rows,
            });
            append_group(
                &call.name,
                scope_ref,
                startup,
                "startup",
                None,
                0,
                &mut blocks,
            );
        } else if let Some(src) = source {
            let group_id = scope_ref.unwrap_or(&call.name);
            let heading = (call.name == "ls").then(|| "目录概况".to_owned());
            append_group(
                &call.name,
                scope_ref,
                src,
                group_id,
                heading,
                0,
                &mut blocks,
            );
        }
        if !same_map_ptr(source, detail)
            && let Some(d) = detail
        {
            let filtered: Map<String, Value> = d
                .iter()
                .filter(|(k, _)| k.contains("Notice") || k.as_str() == "degraded")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            append_group(
                &call.name,
                scope_ref,
                &filtered,
                "collection",
                None,
                0,
                &mut blocks,
            );
        }
        if let Some(src) = source {
            push_notices(&call.name, scope_ref, src, &mut blocks);
        }
        if !same_map_ptr(source, detail)
            && let Some(d) = detail
        {
            push_notices(&call.name, scope_ref, d, &mut blocks);
        }
    } else {
        if let Some(d) = detail {
            append_group(&call.name, scope_ref, d, "result", None, 0, &mut blocks);
        }
        if !raw.is_empty() {
            blocks.push(ToolBlock::Text {
                id: "content".into(),
                text: raw.clone(),
            });
        }
        if let Some(d) = detail {
            push_notices(&call.name, scope_ref, d, &mut blocks);
        }
    }

    let image_blocks: Vec<ToolBlock> = result
        .images
        .iter()
        .enumerate()
        .map(|(idx, (mime, data))| ToolBlock::Image {
            id: format!("image-{idx}"),
            mime_type: mime.clone(),
            data: data.clone(),
        })
        .collect();
    if call.name == "read" {
        let mut combined = image_blocks;
        combined.extend(blocks);
        blocks = combined;
    } else {
        blocks.extend(image_blocks);
    }

    if blocks.is_empty() {
        blocks.push(ToolBlock::Text {
            id: "fallback".into(),
            text: if raw.is_empty() {
                "结果为空".into()
            } else {
                raw.clone()
            },
        });
    }

    ToolCardView {
        status,
        blocks,
        raw,
    }
}

fn same_map_ptr(a: Option<&Map<String, Value>>, b: Option<&Map<String, Value>>) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => std::ptr::eq(x, y),
        (None, None) => true,
        _ => false,
    }
}

fn append_group(
    tool_name: &str,
    scope: Option<&str>,
    data: &Map<String, Value>,
    id: &str,
    heading: Option<String>,
    depth: usize,
    blocks: &mut Vec<ToolBlock>,
) {
    if depth > 2 {
        return;
    }
    let start = blocks.len();
    let label = |key: &str| resolve_label(tool_name, scope, key);
    let mut rows: Vec<FieldRow> = Vec::new();

    for (key, value) in data {
        if is_notice_key(key) || is_hidden_key(tool_name, key) {
            continue;
        }
        if tool_name == "eventlog"
            && scope == Some("detail")
            && key == "msg"
            && let Some(s) = value.as_str()
        {
            blocks.push(ToolBlock::Text {
                id: format!("{id}-message"),
                text: s.to_owned(),
            });
            continue;
        }
        if key == "stats" && value.is_object() {
            continue;
        }
        if let Some(arr) = value.as_array() {
            if arr.is_empty() {
                blocks.push(ToolBlock::Notice {
                    id: format!("{id}-{key}-empty"),
                    text: format!("{}：本次没有结果", label(key)),
                    status: NoticeStatus::Default,
                });
                continue;
            }
            if key == "passages" {
                for (idx, item) in arr.iter().enumerate() {
                    let passage = item.as_object();
                    let text_val = words(passage.and_then(|p| p.get("text")));
                    let src = passage
                        .and_then(|p| p.get("source_url"))
                        .filter(|v| !v.is_null());
                    let text = match src {
                        Some(u) => format!("{text_val}\n\n来源：{}", words(Some(u))),
                        None => text_val,
                    };
                    blocks.push(ToolBlock::Text {
                        id: format!("{id}-{key}-{idx}"),
                        text,
                    });
                }
                continue;
            }
            if is_ranking_key(key) {
                let items = arr
                    .iter()
                    .enumerate()
                    .map(|(idx, item)| match item.as_object() {
                        Some(entry) => ListingItem {
                            name: ranking_name_of(key, entry, idx),
                            metrics: ranking_metrics_of(tool_name, scope, key, entry),
                        },
                        None => ListingItem {
                            name: (idx + 1).to_string(),
                            metrics: vec![FieldRow {
                                label: "结果".into(),
                                value: words(Some(item)),
                            }],
                        },
                    })
                    .collect();
                blocks.push(ToolBlock::Listing {
                    id: format!("{id}-{key}"),
                    items,
                });
                continue;
            }
            for (idx, item) in arr.iter().enumerate() {
                let Some(entry) = item.as_object() else {
                    blocks.push(ToolBlock::Text {
                        id: format!("{id}-{key}-{idx}"),
                        text: words(Some(item)),
                    });
                    continue;
                };
                let mut entry_rows = build_entry_rows(tool_name, scope, key, entry);
                append_merged_volume_rows(tool_name, scope, entry, &mut entry_rows);
                blocks.push(ToolBlock::Fields {
                    id: format!("{id}-{key}-{idx}"),
                    title: group_heading(key, entry, idx),
                    rows: entry_rows,
                });
            }
            continue;
        }
        if key == "filesScanned" || key == "elapsedSec" {
            continue;
        }
        if let Some(sub) = value.as_object()
            && depth < 2
        {
            append_group(
                tool_name,
                scope,
                sub,
                &format!("{id}-{key}"),
                Some(label(key)),
                depth + 1,
                blocks,
            );
            continue;
        }
        if value.is_null() && is_nullable_key(key) {
            continue;
        }
        if !value.is_object() && !value.is_array() {
            rows.push(FieldRow {
                label: label(key),
                value: measure(key, Some(value)),
            });
        }
    }

    if !rows.is_empty() {
        blocks.insert(
            start,
            ToolBlock::Fields {
                id: id.to_owned(),
                title: heading.clone(),
                rows,
            },
        );
    }

    if let Some(stats) = data.get("stats").and_then(Value::as_object) {
        let stat_row = FieldRow {
            label: "扫描统计".into(),
            value: format!(
                "{} 个文件 · 用时 {}s",
                words(stats.get("filesScanned")),
                words(stats.get("elapsedSec"))
            ),
        };
        if let Some(ToolBlock::Fields { rows: r, .. }) = blocks
            .iter_mut()
            .find(|b| matches!(b, ToolBlock::Fields { id: bid, .. } if bid == id))
        {
            r.push(stat_row);
        } else {
            blocks.insert(
                start,
                ToolBlock::Fields {
                    id: id.to_owned(),
                    title: heading,
                    rows: vec![stat_row],
                },
            );
        }
    }
}

fn ranking_name_of(key: &str, entry: &Map<String, Value>, index: usize) -> String {
    if key == "counts" {
        let prov = entry
            .get("provider")
            .filter(|v| !v.is_null())
            .map(|v| words(Some(v)))
            .unwrap_or_else(|| "未知来源".into());
        let id = entry
            .get("id")
            .filter(|v| !v.is_null())
            .map(|v| words(Some(v)))
            .unwrap_or_else(|| "-".into());
        return format!("{prov} / {id}");
    }
    for field in NAME_KEYS {
        if let Some(v) = entry.get(*field).filter(|v| !v.is_null()) {
            return words(Some(v));
        }
    }
    (index + 1).to_string()
}

fn ranking_metrics_of(
    tool_name: &str,
    scope: Option<&str>,
    key: &str,
    entry: &Map<String, Value>,
) -> Vec<FieldRow> {
    if key == "counts" {
        return [
            FieldRow {
                label: "次数".into(),
                value: measure("n", entry.get("n")),
            },
            FieldRow {
                label: "最近一次".into(),
                value: measure("last", entry.get("last")),
            },
        ]
        .into_iter()
        .filter(|m| m.value != "未知")
        .collect();
    }
    if key == "sources" {
        let fetched_default = Value::Bool(false);
        let fetched = entry.get("fetched").unwrap_or(&fetched_default);
        return vec![
            FieldRow {
                label: "序号".into(),
                value: words(entry.get("rank")),
            },
            FieldRow {
                label: "取得正文".into(),
                value: measure("fetched", Some(fetched)),
            },
        ];
    }
    let has_size = entry.get("size").is_some_and(|v| !v.is_null());
    entry
        .iter()
        .filter(|(field, _)| {
            let f = field.as_str();
            !NAME_KEYS.contains(&f)
                && !matches!(f, "type" | "pid" | "engtypes" | "ext")
                && (f != "bytes" || !has_size)
        })
        .map(|(field, value)| FieldRow {
            label: resolve_label(tool_name, scope, field),
            value: measure(field, Some(value)),
        })
        .collect()
}

fn build_entry_rows(
    tool_name: &str,
    scope: Option<&str>,
    key: &str,
    entry: &Map<String, Value>,
) -> Vec<FieldRow> {
    if key == "startupFolders" {
        let mut rows = vec![FieldRow {
            label: "路径".into(),
            value: words(entry.get("path")),
        }];
        if let Some(items) = entry.get("items").and_then(Value::as_array) {
            for item in items {
                if let Some(file) = item.as_object() {
                    rows.push(FieldRow {
                        label: words(file.get("name")),
                        value: measure("disabled", file.get("disabled")),
                    });
                }
            }
        }
        return rows;
    }
    entry
        .iter()
        .filter(|(field, _)| {
            let f = field.as_str();
            if is_notice_key(f) || is_hidden_key(tool_name, f) {
                return false;
            }
            if (key == "space" || key == "volumes") && f == "drive" {
                return false;
            }
            if key == "regItems" && f == "name" {
                return false;
            }
            if key == "services" && f == "display" {
                return false;
            }
            true
        })
        .map(|(field, value)| FieldRow {
            label: resolve_label(tool_name, scope, field),
            value: if field == "label" && value.is_null() {
                "（无卷标）".into()
            } else {
                measure(field, Some(value))
            },
        })
        .collect()
}

fn append_merged_volume_rows(
    tool_name: &str,
    scope: Option<&str>,
    entry: &Map<String, Value>,
    rows: &mut Vec<FieldRow>,
) {
    let has_free = entry.get("freeGB").is_some_and(|v| !v.is_null());
    let has_used = entry.get("usedPct").is_some_and(|v| !v.is_null());
    if !has_free && !has_used {
        return;
    }
    let free_label = resolve_label(tool_name, scope, "freeGB");
    let used_label = resolve_label(tool_name, scope, "usedPct");
    let free_idx = rows.iter().position(|r| r.label == free_label);
    let used_idx = rows.iter().position(|r| r.label == used_label);
    if free_idx.is_none() && used_idx.is_none() {
        return;
    }
    let mut parts = Vec::new();
    if free_idx.is_some() {
        parts.push(measure("freeGB", entry.get("freeGB")));
    }
    if used_idx.is_some() {
        parts.push(format!("已用 {}", measure("usedPct", entry.get("usedPct"))));
    }
    let mut remove_indices: Vec<usize> = [used_idx, free_idx].into_iter().flatten().collect();
    remove_indices.sort_unstable_by(|a, b| b.cmp(a));
    for idx in remove_indices {
        rows.remove(idx);
    }
    rows.push(FieldRow {
        label: "剩余".into(),
        value: parts.join(" · "),
    });
}

fn push_notices(
    tool_name: &str,
    scope: Option<&str>,
    data: &Map<String, Value>,
    blocks: &mut Vec<ToolBlock>,
) {
    let at = blocks.len();
    let label = |key: &str| resolve_label(tool_name, scope, key);
    if let Some(deg) = data.get("degradedFrom").filter(|v| !v.is_null()) {
        blocks.push(ToolBlock::Notice {
            id: format!("degraded-{at}"),
            text: format!(
                "WizTree 不可用，已降级为逐文件统计（{}）。数值为下界。",
                words(Some(deg))
            ),
            status: NoticeStatus::Warning,
        });
    }
    for key in [
        "smartNotice",
        "infoNotice",
        "notice",
        "outputNotice",
        "truncation",
        "omitted",
        "collectionErrors",
        "counterErrors",
        "smartErrors",
    ] {
        let Some(val) = data.get(key).filter(|v| !v.is_null()) else {
            continue;
        };
        if key == "truncation" {
            let Some(tr) = val.as_object() else {
                continue;
            };
            if !tr.get("truncated").is_some_and(is_js_truthy) {
                continue;
            }
            let by = match tr.get("truncatedBy").and_then(Value::as_str) {
                Some("lines") => "行".to_owned(),
                _ => words(tr.get("truncatedBy")),
            };
            blocks.push(ToolBlock::Notice {
                id: format!("truncation-{at}"),
                text: format!(
                    "{by}数超出上限，输出 {} / {}。",
                    words(tr.get("outputLines")),
                    words(tr.get("totalLines"))
                ),
                status: NoticeStatus::Warning,
            });
            continue;
        }
        if key == "omitted" {
            if let Some(om) = val.as_object() {
                let mut parts = vec![words(om.get("note"))];
                if let Some(c) = om.get("count").filter(|v| !v.is_null()) {
                    parts.push(format!("已省略 {} 项", words(Some(c))));
                }
                if let Some(s) = om.get("size").filter(|v| !v.is_null()) {
                    parts.push(format!("合计 {}", words(Some(s))));
                }
                if let Some(u) = om.get("unknownCount").filter(|v| is_js_truthy(v)) {
                    parts.push(format!("其中 {} 项大小未知", words(Some(u))));
                }
                let joined = parts
                    .into_iter()
                    .filter(|p| !p.is_empty())
                    .collect::<Vec<_>>()
                    .join("，")
                    .replace("，已省略", "。已省略");
                blocks.push(ToolBlock::Notice {
                    id: format!("omitted-{at}"),
                    text: format!("{joined}。"),
                    status: NoticeStatus::Default,
                });
            }
            continue;
        }
        if matches!(key, "collectionErrors" | "counterErrors" | "smartErrors") {
            let items: Vec<&Value> = match val.as_array() {
                Some(arr) => arr.iter().collect(),
                None => vec![val],
            };
            for item in items {
                let line = if let Some(entry) = item.as_object() {
                    let body = entry
                        .get("error")
                        .or_else(|| entry.get("message"))
                        .or_else(|| entry.get("msg"))
                        .unwrap_or(item);
                    let scope_val = entry
                        .get("deviceId")
                        .or_else(|| entry.get("name"))
                        .or_else(|| entry.get("provider"))
                        .filter(|v| !v.is_null());
                    match scope_val {
                        Some(sv) => {
                            format!(
                                "{}：{} · {}",
                                label(key),
                                words(Some(sv)),
                                words(Some(body))
                            )
                        }
                        None => format!("{}：{}", label(key), words(Some(body))),
                    }
                } else {
                    format!("{}：{}", label(key), words(Some(item)))
                };
                blocks.push(ToolBlock::Notice {
                    id: format!("{key}-{at}-{}", line.chars().count()),
                    text: line,
                    status: NoticeStatus::Warning,
                });
            }
            continue;
        }
        blocks.push(ToolBlock::Notice {
            id: format!("{key}-{at}"),
            text: words(Some(val)),
            status: if key == "notice" {
                NoticeStatus::Default
            } else {
                NoticeStatus::Warning
            },
        });
    }
    if data.get("outputTruncated").is_some_and(is_js_truthy)
        || data.get("unreadable").is_some_and(is_js_truthy)
    {
        blocks.push(ToolBlock::Notice {
            id: format!("cut-{at}"),
            text: "结果已截断，列表只代表本次返回的范围。".into(),
            status: NoticeStatus::Warning,
        });
    }
}

pub fn conversion_label(code: Option<i64>) -> String {
    match code {
        None => "状态未确认".into(),
        Some(0) => "完全解密".into(),
        Some(1) => "已加密".into(),
        Some(2) => "加密中".into(),
        Some(3) => "解密中".into(),
        Some(4) => "加密暂停".into(),
        Some(5) => "解密暂停".into(),
        Some(other) => format!("未知状态 ({other})"),
    }
}

pub fn lock_label(code: Option<i64>) -> &'static str {
    match code {
        Some(0) => "已解锁",
        Some(1) => "已锁定",
        _ => "锁定状态未确认",
    }
}

fn special_error_text(val: &Value) -> String {
    if val.is_null() {
        return String::new();
    }
    if let Some(s) = val.as_str() {
        return s.to_owned();
    }
    if let Some(obj) = val.as_object() {
        let source = obj.get("source").and_then(Value::as_str).unwrap_or("");
        let code = obj
            .get("code")
            .and_then(Value::as_i64)
            .map(|c| format!("0x{:08X}", c as u32))
            .unwrap_or_default();
        let msg = obj.get("message").and_then(Value::as_str).unwrap_or("");
        let parts: Vec<&str> = [source, code.as_str(), msg]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        if !parts.is_empty() {
            return parts.join(" · ");
        }
        return serde_json::to_string(obj).unwrap_or_default();
    }
    words(Some(val))
}

fn collect_special_errors(values: &[Option<&Value>]) -> Vec<String> {
    let mut out = Vec::new();
    for val in values.iter().flatten() {
        if let Some(arr) = val.as_array() {
            for item in arr {
                let s = special_error_text(item);
                if !s.is_empty() {
                    out.push(s);
                }
            }
        } else {
            let s = special_error_text(val);
            if !s.is_empty() {
                out.push(s);
            }
        }
    }
    out
}

fn format_gb_from_mb(mb: Option<&Value>) -> String {
    let Some(num) = mb.and_then(Value::as_f64).filter(|f| f.is_finite()) else {
        return "未知".into();
    };
    let formatted = format!("{:.1}", num / 1024.0);
    formatted
        .strip_suffix(".0")
        .unwrap_or(&formatted)
        .to_owned()
}

/// Projects any of the 4 special persistent tool components (`sys(overview)`, `runbook` / `command_plan` / `runbook_deliver`,
/// `web_search`, `bitlocker(unlock/decrypt)`), matching `src/web/frontend/app/cards/special-tool-data.ts` 1-to-1.
pub fn map_special_tool(
    call: &ToolCallInput,
    result: Option<&ToolResultInput>,
    live: bool,
) -> Option<SpecialToolView> {
    let kind = special_tool_kind(call)?;
    let details = result
        .and_then(|r| r.details.as_ref())
        .and_then(Value::as_object);
    let raw = result.map(|r| r.raw_text.clone()).unwrap_or_default();
    let started = call.execution.as_ref().and_then(|e| e.started_at).is_some();
    let initial = SpecialStatus::from(compute_tool_state(
        result,
        Some(CardStatus::Success),
        live,
        started,
    ));
    let raw_err_val = result
        .filter(|r| r.is_error && !raw.is_empty())
        .map(|_| Value::String(raw.clone()));
    let common_errors =
        collect_special_errors(&[details.and_then(|d| d.get("error")), raw_err_val.as_ref()]);
    let base_summary = match initial {
        SpecialStatus::Pending => "等待执行",
        SpecialStatus::Running => "执行中",
        SpecialStatus::Interrupted => "未完成或已中断",
        _ => "已完成",
    };

    match kind {
        SpecialToolKind::SysOverview => {
            let source = details
                .and_then(|d| d.get("overview"))
                .and_then(Value::as_object);
            let mem = source.and_then(|s| s.get("mem")).and_then(Value::as_object);
            let machine_obj = source
                .and_then(|s| s.get("machine"))
                .and_then(Value::as_object);
            let uptime_obj = source
                .and_then(|s| s.get("uptime"))
                .and_then(Value::as_object);
            let mut errors = common_errors;
            errors.extend(collect_special_errors(&[
                source.and_then(|s| s.get("error")),
                source.and_then(|s| s.get("collectionErrors")),
                source.and_then(|s| s.get("counterErrors")),
            ]));

            let cpu = source
                .and_then(|s| s.get("cpuTotalPct"))
                .and_then(Value::as_f64)
                .filter(|f| f.is_finite());
            let memory = mem
                .and_then(|m| m.get("usedPct"))
                .and_then(Value::as_f64)
                .filter(|f| f.is_finite());
            let cores = source
                .and_then(|s| s.get("logicalCores"))
                .and_then(Value::as_i64);

            let status = match result {
                None => initial,
                Some(res)
                    if res.is_error
                        || source
                            .and_then(|s| s.get("error"))
                            .is_some_and(|v| !v.is_null()) =>
                {
                    SpecialStatus::Error
                }
                Some(_)
                    if source.is_none()
                        || source
                            .and_then(|s| s.get("degraded"))
                            .is_some_and(is_js_truthy)
                        || !errors.is_empty()
                        || cpu.is_none()
                        || memory.is_none() =>
                {
                    SpecialStatus::Degraded
                }
                Some(_) => SpecialStatus::Success,
            };

            let vendor = machine_obj
                .and_then(|m| m.get("vendor"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let model_str = machine_obj
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let cpu_name = machine_obj
                .and_then(|m| m.get("cpu"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let machine_field = {
                let s = [vendor, model_str]
                    .into_iter()
                    .filter(|p| !p.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · ");
                if s.is_empty() { "未知".into() } else { s }
            };
            let pool_obj = source
                .and_then(|s| s.get("pool"))
                .and_then(Value::as_object);
            let pool_val = match pool_obj
                .and_then(|p| p.get("nonpagedMB"))
                .filter(|v| !v.is_null())
            {
                None => "未知".into(),
                Some(np) => format!(
                    "{} MB 非分页 / {} MB 分页",
                    words(Some(np)),
                    words(pool_obj.and_then(|p| p.get("pagedMB")))
                ),
            };
            let mut fields = vec![
                FieldRow {
                    label: "机型".into(),
                    value: machine_field,
                },
                FieldRow {
                    label: "处理器".into(),
                    value: words(machine_obj.and_then(|m| m.get("cpu"))),
                },
                FieldRow {
                    label: "逻辑核心".into(),
                    value: words(source.and_then(|s| s.get("logicalCores"))),
                },
                FieldRow {
                    label: "内核池".into(),
                    value: pool_val,
                },
            ];
            if let Some(pfs) = source
                .and_then(|s| s.get("pagefile"))
                .and_then(Value::as_array)
            {
                for pf in pfs.iter().filter_map(Value::as_object) {
                    let name = pf
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("页面文件")
                        .to_owned();
                    fields.push(FieldRow {
                        label: name,
                        value: format!(
                            "{} / {} GB",
                            format_gb_from_mb(pf.get("usedMB")),
                            format_gb_from_mb(pf.get("allocMB"))
                        ),
                    });
                }
            }
            fields.push(FieldRow {
                label: "开机时间".into(),
                value: words(uptime_obj.and_then(|u| u.get("bootTime"))),
            });

            let summary = if result.is_none() {
                if base_summary == "执行中" {
                    "正在采集"
                } else {
                    base_summary
                }
            } else {
                match status {
                    SpecialStatus::Error => "采集失败",
                    SpecialStatus::Degraded => "部分数据缺失",
                    _ if live => "刚刚采集",
                    _ => "已采集",
                }
            }
            .to_owned();

            let total_mem_str = mem
                .and_then(|m| m.get("totalMB"))
                .filter(|v| !v.is_null())
                .map(|v| format!("{} GB 内存", format_gb_from_mb(Some(v))))
                .unwrap_or_default();
            let machine = [vendor, cpu_name, total_mem_str.as_str()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            let capacity = match mem {
                None => "容量未知".into(),
                Some(m) => format!(
                    "{} / {} GB",
                    format_gb_from_mb(m.get("usedMB")),
                    format_gb_from_mb(m.get("totalMB"))
                ),
            };
            let uptime = uptime_obj
                .and_then(|u| u.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();

            Some(SpecialToolView::SysOverview {
                status,
                title: "整机概览".into(),
                summary,
                raw,
                fields: if status == SpecialStatus::Error {
                    Vec::new()
                } else {
                    fields
                },
                errors,
                machine,
                cpu,
                cores,
                memory,
                capacity,
                uptime,
            })
        }
        SpecialToolKind::Runbook => {
            let parsed_raw: Option<Value> = if details
                .and_then(|d| d.get("runbook"))
                .and_then(Value::as_object)
                .is_none()
                && result.is_some_and(|r| !r.is_error)
            {
                serde_json::from_str::<Value>(&raw).ok()
            } else {
                None
            };
            let source = details
                .and_then(|d| d.get("runbook"))
                .and_then(Value::as_object)
                .or_else(|| {
                    parsed_raw
                        .as_ref()
                        .and_then(Value::as_object)
                        .and_then(|o| o.get("runbook"))
                        .and_then(Value::as_object)
                });
            let mut errors = common_errors;
            errors.extend(collect_special_errors(&[
                source.and_then(|s| s.get("error"))
            ]));
            let status = match result {
                None => initial,
                Some(res) if res.is_error || !errors.is_empty() => SpecialStatus::Error,
                Some(_) if source.is_none() => SpecialStatus::Degraded,
                Some(_) => SpecialStatus::Success,
            };
            let delivery = if result.is_some()
                && status == SpecialStatus::Success
                && let Some(file) = source.and_then(|s| s.get("file")).and_then(Value::as_str)
            {
                Some(RunbookDeliveryView {
                    file: file.to_owned(),
                    name: source
                        .and_then(|s| s.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or("命令清单.txt")
                        .to_owned(),
                    level: source
                        .and_then(|s| s.get("levelLabel"))
                        .and_then(Value::as_str)
                        .unwrap_or("风险档未知")
                        .to_owned(),
                    count: source.and_then(|s| s.get("items")).and_then(Value::as_i64),
                })
            } else {
                None
            };

            let cmd_array = source
                .and_then(|s| s.get("commands"))
                .and_then(Value::as_array)
                .or_else(|| call.arguments.get("items").and_then(Value::as_array));
            let commands: Vec<CommandItem> = cmd_array
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_object)
                        .map(|item| CommandItem {
                            shell: item
                                .get("shell")
                                .and_then(Value::as_str)
                                .unwrap_or("环境未知")
                                .to_owned(),
                            admin: item.get("admin").and_then(Value::as_bool) == Some(true),
                            summary: item
                                .get("summary")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                            command: item
                                .get("command")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                        })
                        .collect()
                })
                .unwrap_or_default();

            let planned =
                source.and_then(|s| s.get("action")).and_then(Value::as_str) == Some("planned");
            let summary = if result.is_none() {
                base_summary
            } else if status == SpecialStatus::Error {
                "生成失败"
            } else if planned {
                "方案已保存"
            } else if delivery.is_some() {
                "已生成"
            } else {
                "交付结果缺失"
            }
            .to_owned();

            let title = source
                .and_then(|s| s.get("title"))
                .and_then(Value::as_str)
                .or_else(|| call.arguments.get("title").and_then(Value::as_str))
                .unwrap_or("命令清单")
                .to_owned();
            let count = source
                .and_then(|s| s.get("items"))
                .and_then(Value::as_i64)
                .or_else(|| (!commands.is_empty()).then_some(commands.len() as i64));
            let fields = delivery
                .as_ref()
                .map(|d| {
                    vec![FieldRow {
                        label: "文件路径".into(),
                        value: d.file.clone(),
                    }]
                })
                .unwrap_or_default();

            Some(SpecialToolView::Runbook {
                status,
                title,
                summary,
                raw,
                fields,
                errors,
                commands,
                level: source
                    .and_then(|s| s.get("levelLabel"))
                    .and_then(Value::as_str)
                    .unwrap_or("风险档未知")
                    .to_owned(),
                count,
                version: source
                    .and_then(|s| s.get("version"))
                    .and_then(Value::as_i64),
                planned,
                delivery,
            })
        }
        SpecialToolKind::WebSearch => {
            let queries: Vec<String> = match call
                .arguments
                .get("queries")
                .or_else(|| call.arguments.get("query"))
            {
                Some(Value::Array(arr)) => arr
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                Some(Value::String(s)) => vec![s.clone()],
                _ => Vec::new(),
            };
            let successful = details
                .and_then(|d| d.get("successfulQueries"))
                .and_then(Value::as_i64);
            let count = details
                .and_then(|d| d.get("queryCount"))
                .and_then(Value::as_i64)
                .unwrap_or(queries.len() as i64);
            let partial = successful.is_some_and(|s| s < count);
            let failed = successful == Some(0) && count > 0;
            let mut errors = common_errors;
            if let Some(q_arr) = details
                .and_then(|d| d.get("queries"))
                .and_then(Value::as_array)
            {
                for q in q_arr.iter().filter_map(Value::as_object) {
                    errors.extend(collect_special_errors(&[q.get("error")]));
                }
            }
            let status = match result {
                None => initial,
                Some(res)
                    if res.is_error
                        || details
                            .and_then(|d| d.get("error"))
                            .is_some_and(|v| !v.is_null())
                        || failed =>
                {
                    SpecialStatus::Error
                }
                Some(_) if partial || details.is_none() => SpecialStatus::Degraded,
                Some(_) => SpecialStatus::Success,
            };
            let summary = if result.is_none() {
                match initial {
                    SpecialStatus::Running => "正在联网检索…",
                    SpecialStatus::Pending => "等待联网检索",
                    _ => "联网检索未完成",
                }
            } else {
                match status {
                    SpecialStatus::Error => "联网检索失败",
                    SpecialStatus::Degraded if details.is_some() => "部分检索完成",
                    SpecialStatus::Degraded => "检索状态未确认",
                    _ => "已联网检索",
                }
            }
            .to_owned();
            if errors.is_empty() {
                if failed {
                    errors.push("全部查询失败".into());
                } else if partial {
                    errors.push(format!("{} 项查询未完成", count - successful.unwrap_or(0)));
                }
            }

            Some(SpecialToolView::WebSearch {
                status,
                title: "联网检索".into(),
                summary,
                raw,
                fields: Vec::new(),
                errors,
                queries,
                successful,
                unstructured: details.is_none(),
            })
        }
        SpecialToolKind::BitLocker => {
            let source = details
                .and_then(|d| d.get("bitlocker"))
                .and_then(Value::as_object);
            let top_op = source
                .and_then(|s| s.get("operation"))
                .and_then(Value::as_object);
            let scope = if call.arguments.get("scope").and_then(Value::as_str) == Some("unlock") {
                "unlock"
            } else {
                "decrypt"
            };
            let raw_drive = call
                .arguments
                .get("drive")
                .and_then(Value::as_str)
                .unwrap_or("");
            let normalized_drive = if (raw_drive.len() == 1
                && raw_drive.as_bytes()[0].is_ascii_alphabetic())
                || (raw_drive.len() == 2
                    && raw_drive.as_bytes()[0].is_ascii_alphabetic()
                    && raw_drive.as_bytes()[1] == b':')
            {
                format!(
                    "{}:",
                    (raw_drive.as_bytes()[0] as char).to_ascii_uppercase()
                )
            } else {
                "目标待确定".into()
            };

            let volumes: Vec<SpecialVolumeView> = source
                .and_then(|s| s.get("volumes"))
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_object)
                        .map(|v| {
                            let op = v.get("operation").and_then(Value::as_object);
                            let conv_code = v.get("conversionStatus").and_then(Value::as_i64);
                            let lock_code = v.get("lockStatus").and_then(Value::as_i64);
                            let ret_code_obj = op
                                .and_then(|o| o.get("returnCode"))
                                .filter(|rc| is_js_truthy(rc))
                                .map(|rc| {
                                    serde_json::json!({
                                        "source": "方法返回码",
                                        "code": rc,
                                    })
                                });
                            let vol_errors = collect_special_errors(&[
                                op.and_then(|o| o.get("error")),
                                v.get("collectionErrors"),
                                ret_code_obj.as_ref(),
                            ]);
                            SpecialVolumeView {
                                id: v
                                    .get("volumeId")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_owned(),
                                drive: v
                                    .get("drive")
                                    .and_then(Value::as_str)
                                    .unwrap_or("无盘符")
                                    .to_owned(),
                                conversion: conversion_label(conv_code),
                                conversion_code: conv_code,
                                lock: lock_label(lock_code).to_owned(),
                                percentage: v
                                    .get("encryptionPercentage")
                                    .and_then(Value::as_f64)
                                    .filter(|f| f.is_finite()),
                                operation: op
                                    .and_then(|o| o.get("status"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_owned(),
                                reason: op
                                    .and_then(|o| o.get("reason"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_owned(),
                                errors: vol_errors,
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();

            let top_reason = top_op
                .and_then(|o| o.get("reason"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let top_reason_val = (volumes.is_empty() && !top_reason.is_empty())
                .then(|| Value::String(top_reason.clone()));
            let mut errors = common_errors;
            errors.extend(collect_special_errors(&[
                source.and_then(|s| s.get("collectionErrors")),
                top_reason_val.as_ref(),
            ]));
            for vol in &volumes {
                errors.extend(vol.errors.iter().cloned());
            }

            let attempted: Vec<&SpecialVolumeView> = volumes
                .iter()
                .filter(|v| v.operation != "skipped")
                .collect();
            let mut status = initial;
            let mut summary = if initial == SpecialStatus::Running {
                match call
                    .execution
                    .as_ref()
                    .and_then(|e| e.stage.as_deref())
                    .unwrap_or("")
                {
                    "正在查询 BitLocker 状态" => "检查卷",
                    "等待确认解密目标" => "等待确认",
                    "等待本地解锁输入或确认" => "等待输入",
                    "正在解锁并复查状态" => "解锁中",
                    "正在提交解密并复查状态" => "提交解密中",
                    _ => base_summary,
                }
                .to_owned()
            } else {
                base_summary.to_owned()
            };

            if let Some(res) = result {
                let top_status = top_op
                    .and_then(|o| o.get("status"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let source_degraded = source
                    .and_then(|s| s.get("degraded"))
                    .is_some_and(is_js_truthy);
                if res.is_error {
                    status = SpecialStatus::Error;
                    summary = "执行失败".into();
                } else if top_status == "unknown"
                    || volumes.iter().any(|v| v.operation == "unknown")
                {
                    status = SpecialStatus::Unknown;
                    summary = "结果未确认".into();
                } else if attempted.iter().any(|v| v.operation == "cancelled") {
                    let all_cancelled = attempted.iter().all(|v| v.operation == "cancelled");
                    let missing = source_degraded || !errors.is_empty();
                    status = if all_cancelled && !missing {
                        SpecialStatus::Cancelled
                    } else {
                        SpecialStatus::Degraded
                    };
                    summary = if all_cancelled {
                        if missing {
                            "已取消，部分状态缺失"
                        } else {
                            "已取消"
                        }
                    } else {
                        "部分处理已取消"
                    }
                    .into();
                } else if !attempted.is_empty()
                    && attempted.iter().all(|v| v.operation == "rejected")
                {
                    status = SpecialStatus::Error;
                    summary = if scope == "unlock" {
                        "解锁失败"
                    } else {
                        "请求被拒绝"
                    }
                    .into();
                } else if source.is_none()
                    || source_degraded
                    || !errors.is_empty()
                    || volumes.iter().any(|v| {
                        v.operation == "rejected"
                            || v.lock == "锁定状态未确认"
                            || !matches!(
                                v.operation.as_str(),
                                "accepted" | "skipped" | "cancelled" | "unknown" | "rejected"
                            )
                    })
                {
                    status = SpecialStatus::Degraded;
                    summary = if source.is_none() {
                        "结果缺少状态信息"
                    } else if volumes.iter().any(|v| v.operation == "rejected") {
                        "部分请求失败"
                    } else if volumes
                        .iter()
                        .any(|v| !matches!(v.operation.as_str(), "accepted" | "skipped"))
                    {
                        "请求结果缺失"
                    } else if scope == "unlock"
                        && !volumes.is_empty()
                        && volumes.iter().all(|v| v.lock == "已解锁")
                    {
                        "已解锁，部分状态缺失"
                    } else if scope == "decrypt"
                        && !volumes.is_empty()
                        && volumes.iter().all(|v| v.conversion_code == Some(0))
                    {
                        "已完全解密，部分状态缺失"
                    } else if scope == "unlock"
                        && volumes.iter().any(|v| v.lock == "锁定状态未确认")
                    {
                        "锁定状态未确认"
                    } else {
                        "部分状态缺失"
                    }
                    .into();
                } else {
                    status = SpecialStatus::Success;
                    summary = if volumes.is_empty() {
                        "无可处理的卷"
                    } else if scope == "unlock" {
                        if volumes.iter().all(|v| v.lock == "已解锁") {
                            "已解锁"
                        } else {
                            "请求已返回"
                        }
                    } else if volumes.iter().all(|v| v.conversion_code == Some(0)) {
                        "已完全解密"
                    } else if volumes.iter().any(|v| v.conversion_code == Some(3)) {
                        "解密中"
                    } else {
                        "请求已返回"
                    }
                    .into();
                }
            }

            let fields: Vec<FieldRow> = if !volumes.is_empty() {
                let collected_at = words(source.and_then(|s| s.get("collectedAt")));
                volumes
                    .iter()
                    .flat_map(|v| {
                        let conv_str = match v.conversion_code {
                            Some(c) => format!("{} ({c})", v.conversion),
                            None => v.conversion.clone(),
                        };
                        vec![
                            FieldRow {
                                label: "目标卷".into(),
                                value: format!("{} · {}", v.drive, v.id),
                            },
                            FieldRow {
                                label: "转换状态".into(),
                                value: conv_str,
                            },
                            FieldRow {
                                label: "锁定状态".into(),
                                value: v.lock.clone(),
                            },
                            FieldRow {
                                label: "请求结果".into(),
                                value: if v.operation.is_empty() {
                                    "未知".into()
                                } else {
                                    v.operation.clone()
                                },
                            },
                            FieldRow {
                                label: "说明".into(),
                                value: v.reason.clone(),
                            },
                            FieldRow {
                                label: "采集时刻".into(),
                                value: collected_at.clone(),
                            },
                        ]
                    })
                    .collect()
            } else if let Some(top) = top_op {
                vec![
                    FieldRow {
                        label: "请求结果".into(),
                        value: top
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("未知")
                            .to_owned(),
                    },
                    FieldRow {
                        label: "说明".into(),
                        value: top_reason,
                    },
                ]
            } else {
                Vec::new()
            };

            let title = if scope == "unlock" {
                format!(
                    "解锁 {}",
                    volumes
                        .first()
                        .map(|v| v.drive.as_str())
                        .unwrap_or(&normalized_drive)
                )
            } else {
                "关闭 BitLocker".into()
            };

            Some(SpecialToolView::BitLocker {
                status,
                title,
                summary,
                raw,
                fields,
                errors,
                scope: scope.to_owned(),
                volumes,
            })
        }
    }
}

/// Revision-keyed presenter cache shared by GUI and TUI views.
#[derive(Default)]
pub struct ToolPresenterCache {
    normal: BTreeMap<(String, u64), Arc<ToolCardView>>,
    special: BTreeMap<(String, u64, bool), Arc<SpecialToolView>>,
}

impl ToolPresenterCache {
    pub fn get_or_map_normal(
        &mut self,
        call: &ToolCallInput,
        result: &ToolResultInput,
        result_revision: u64,
    ) -> Arc<ToolCardView> {
        let key = (call.id.clone(), result_revision);
        if let Some(cached) = self.normal.get(&key) {
            return Arc::clone(cached);
        }
        let mapped = Arc::new(map_tool(call, result));
        if self.normal.len() >= 512 {
            self.normal.clear();
        }
        self.normal.insert(key, Arc::clone(&mapped));
        mapped
    }

    pub fn get_or_map_special(
        &mut self,
        call: &ToolCallInput,
        result: Option<&ToolResultInput>,
        result_revision: u64,
        live: bool,
    ) -> Option<Arc<SpecialToolView>> {
        let key = (call.id.clone(), result_revision, live);
        if let Some(cached) = self.special.get(&key) {
            return Some(Arc::clone(cached));
        }
        let mapped = Arc::new(map_special_tool(call, result, live)?);
        if self.special.len() >= 256 {
            self.special.clear();
        }
        self.special.insert(key, Arc::clone(&mapped));
        Some(mapped)
    }
}

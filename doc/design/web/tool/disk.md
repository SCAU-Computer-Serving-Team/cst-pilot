# disk 工具卡片

数据来自 [disk.ts](../../../../agent/home/extensions/diagnostics/disk.ts) 的实际 `execute` 调用；分支以 [disk-core.ts](../../../../agent/home/extensions/diagnostics/disk-core.ts) 为准。画布：[cst-pilot-tools.pen](../../../../src/web/design/cst-pilot-tools.pen)。以下是 2026-09-23 Windows 普通权限的本机样本。

成功返回 `{content:[{type:"text",text:JSON.stringify(result)}],details:result}`。各 scope 的 JSON 保留 `details` 完整字段，设备序列号局部遮盖；特殊情况中另列相关字段节选。卡片优先读 `details`。模型文本超限时可带 `outputTruncated`，`details` 仍保留完整数据。整次失败时 `execute` 抛错，没有业务 JSON；界面按 `isError` 显示错误文本。

## 各 scope 的返回

### space

`disk({scope:"space",drive:"E"})`：

```json
{"space":[{"drive":"E:","totalGB":331.2,"freeGB":64.9,"usedPct":80.4}]}
```

无 `drive` 时 `space` 含多卷；查无盘符的结果见[特殊情况](#特殊情况)。单卷读取失败会被跳过，不附错误。

### info

`disk({scope:"info",drive:"E"})`：

```json
{
  "physicalDisks":[{"FriendlyName":"WD Blue SN580 1TB","SerialNumber":"E823_8FA6_BF53_0001_xxxx_xxxx_xxxx_xxxx.","MediaType":"SSD","BusType":"NVMe","HealthStatus":"Healthy","OperationalStatus":"OK","DeviceId":"1","sizeGB":931.5}],
  "volumes":[{"drive":"E:","label":"Learn","fs":"NTFS","driveType":"Fixed","totalGB":331.2,"freeGB":64.9}]
}
```

盘符无法关联物理盘时的实测数据见[特殊情况](#特殊情况)。单个数据源失败时，对应字段可为 `{ "error": "…" }`；两个数据源均失败时整次抛错。

### health

`disk({scope:"health",drive:"E"})` 在本机普通权限下整次抛错，无成功 JSON；实测错误原文见[特殊情况](#特殊情况)。

代码中的成功形态为 `smart[]`，每项含设备提供的 `DeviceId`、`Wear`、`Temperature`、`PowerOnHours`、`ReadErrorsTotal`、`WriteErrorsTotal`。部分失败时附 `smartErrors[]`（`deviceId`、`error`）、`smartNotice`、`degraded:true`；成功形态尚未实测。无法关联盘符时整次抛错。`smart:null` 或设备不支持的字段不能显示成正常读数。

### usage

`disk({scope:"usage",path:"E:\\Learning\\Programming\\cst-pilot\\doc\\design",top:1})`，WizTree 实测：

```json
{
  "usage":{
    "method":"wiztree-mft","root":"E:\\Learning\\Programming\\cst-pilot\\doc\\design","totalGB":0.00002,
    "topDirs":[{"path":"E:\\Learning\\Programming\\cst-pilot\\doc\\design\\tool\\","sizeGB":0.00001,"pct":50.7}],
    "topFiles":[{"path":"E:\\Learning\\Programming\\cst-pilot\\doc\\design\\web\\tool\\disk.md","sizeGB":0.000009,"pct":45.8}],
    "extAgg":[{"ext":"md","files":6,"sizeGB":0.00002}],"staleFiles":[],
    "notice":"WizTree 全量 MFT 导出（10 行，其中文件 6 个）。topDirs=目录排行；topFiles=单个大文件；extAgg=按扩展名聚合（含文件数）；staleFiles=≥50MB 且 ≥1 年未修改的文件（大者优先）。全部只读统计。"
  }
}
```

`node-walk` 的实测回退结果见[特殊情况](#特殊情况)。WizTree 对非 NTFS 或文件系统未知的卷返回 `method:"wiztree-walk"`；该分支由代码确认，尚未实测。目录排行可能含父子目录，数值不可相加。

### all

`disk({scope:"all",drive:"E"})`。三个 scope 的字段在同一层；本机 SMART 失败时仍正常返回其余数据：

```json
{
  "space":[{"drive":"E:","totalGB":331.2,"freeGB":64.9,"usedPct":80.4}],
  "physicalDisks":[{"FriendlyName":"WD Blue SN580 1TB","SerialNumber":"E823_8FA6_BF53_0001_xxxx_xxxx_xxxx_xxxx.","MediaType":"SSD","BusType":"NVMe","HealthStatus":"Healthy","OperationalStatus":"OK","DeviceId":"1","sizeGB":931.5}],
  "volumes":[{"drive":"E:","label":"Learn","fs":"NTFS","driveType":"Fixed","totalGB":331.2,"freeGB":64.9}],
  "smart":null,
  "smartErrors":[{"deviceId":"1","error":"无法从客户端中访问 CIM 资源。"}],
  "smartNotice":"SMART 部分或全部采集失败，原因见 smartErrors。检测到访问拒绝，可尝试以管理员身份重试。",
  "degraded":true
}
```

`all` 不含 `usage`。盘符关联失败时可增加 `infoNotice`；此时 `smart:null` 且 `smartNotice` 说明未查询 SMART。

## 主模板与字段组

结果从**概览组、排行组、原文块**中选一种主模板。画布数值只用于排版，实际字段按返回数据生成。等待返回时沿用调用中状态。

| 返回条件 | 主模板 | 主要内容组 | 需额外处理的信息 |
|---|---|---|---|
| `isError`（优先判断） | 原文块 | 实际错误文本 | 没有业务 `details`；不能按 scope 的成功结构读取 |
| `space` | 概览组 | `space[]`，每卷一组 | 空数组；单卷读取失败会被静默跳过 |
| `info` | 概览组 | `volumes[]`、`physicalDisks[]`，每卷、每盘一组 | `infoNotice`、`degraded`；任一数据源可能是 `{error}`，不能当数组遍历 |
| `health` | 概览组 | `smart[]`，每盘一组 | `smart:null`、字段值为 `null`、`smartErrors[]`、`smartNotice`、`degraded`；成功形态待实测 |
| `usage`（WizTree） | 排行组 | `usage.topDirs[]` | `root`、`totalGB`、`method`、`notice`；有数据时追加 `topFiles[]`、`extAgg[]`、`staleFiles[]` 排行组；`wiztree-walk` 也走此项 |
| `usage`（`node-walk`） | 排行组 | `usage.topDirs[]` | `stats`、`truncated`、`denied[]`、`degradedFrom`、`notice`；没有 WizTree 的三类文件列表 |
| `all` | 概览组 | 同层的 `space[]`、`volumes[]`、`physicalDisks[]`、`smart[]` | `infoNotice`、`smartErrors[]`、`smartNotice`、`degraded`；`smart:null` 时仍展示其他结果 |

`content` 可能标记 `outputTruncated`，卡片读取完整 `details`。

## 特殊情况

### 查无盘符与设备关联失败

查询不存在的 Z: 时，`space` 实测返回 `{"space":[]}`。同一盘符的 `info` 实测返回节选 `{"physicalDisks":[{"FriendlyName":"Samsung SSD 970 EVO 1TB","SerialNumber":"0025_3852_xxxx_xxxx."},{"FriendlyName":"WD Blue SN580 1TB","SerialNumber":"E823_8FA6_BF53_0001_xxxx_xxxx_xxxx_xxxx."}],"volumes":[],"infoNotice":"盘符关联失败（无法确定目标卷对应的物理盘），physicalDisks 为未过滤全量清单；volumes 仍按盘符过滤","degraded":true}`。`space` 显示空结果；`info` 保留未过滤设备组，同时标明关联失败，不能把两台物理盘都当作 Z: 的设备。

### SMART 采集失败

`health` 在本机普通权限下实测整次抛错，无业务 `details`：

```text
SMART 部分或全部采集失败，原因见 smartErrors。检测到访问拒绝，可尝试以管理员身份重试。 [{"deviceId":"1","error":"无法从客户端中访问 CIM 资源。"}]
```

主模板改为错误原文块。`all` 则实测保留容量与设备信息，同时返回 `{"smart":null,"smartErrors":[{"deviceId":"1","error":"无法从客户端中访问 CIM 资源。"}],"degraded":true}`。这一组加警告提示，`smart:null` 不显示为健康。

### `usage` 回退

临时副本中模拟 WizTree 无法启动，实测：

```json
{"usage":{"method":"node-walk","totalGB":0.00002,"topDirs":[{"path":"E:\\Learning\\Programming\\cst-pilot\\doc\\design\\tool","sizeGB":0.00001,"pct":50.7}],"stats":{"filesScanned":9,"elapsedSec":0,"budget":500000},"truncated":false,"degradedFrom":"wiztree: spawn UNKNOWN","notice":"逐文件统计完成，数值为遍历所得（系统拒绝访问的目录未计入）。"}}
```

沿用排行组，另附扫描统计和降级原因。`node-walk` 无文件排行、扩展名和旧文件列表；`denied` 只在目录读取被拒绝时出现。WizTree 程序缺失时不会给出 `degradedFrom`。缺少 `path` 或路径不存在会整次抛错，不返回 `usage`。

### 多排行

WizTree 实测 `topDirs[]`、`topFiles[]` 和 `extAgg[]` 各有 1 项，`staleFiles:[]`；上方 [usage 样本](#usage)展示真实数值。有数据的列表各生成一个排行组，空列表保留缺席说明与 `notice`，不补造排行。


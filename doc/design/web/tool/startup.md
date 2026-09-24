# startup 工具卡片

[startup.ts](../../../../agent/home/extensions/diagnostics/startup.ts) 无参数，返回 `details.startup`。2026-09-23 直接调用项目扩展；以下保留本机自启项名称与执行路径；账户名局部遮盖。

## 主模板与字段组

主模板为**概览组**。以下 `fields` 字段组按顺序生成，数组项重复使用相同组模板。

| 字段组标题 | 生成规则 | 字段行 |
|---|---|---|
| 自启概况 | 一组 | `regItems.length`、`startupFolders.length`、`services.length` |
| 注册表自启 · `name` | 每个 `regItems[]` 一组 | `source`、`command`、`disabled` |
| 启动文件夹 · `scope` | 每个 `startupFolders[]` 一组 | `path`；每个 `items[].name` 对应一行 `disabled` 状态 |
| 自启服务 · `display` | 每个 `services[]` 一组 | `name`、`state`、`path`；`display` 缺失时标题用 `name` |

实测数组为 28 条注册表项、2 个启动文件夹、101 条自启服务。样本只列每类一项：

```json
{"startup":{"regItems":[{"source":"HKLM\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run","name":"SecurityHealth","command":"C:\\WINDOWS\\system32\\SecurityHealthSystray.exe","disabled":false}],"startupFolders":[{"scope":"user","path":"C:\\Users\\Timxxxx\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs\\Startup","items":[{"name":"mouse-map.ahk.lnk","disabled":null},{"name":"Snipaste.lnk","disabled":true}]}],"services":[{"name":"AudioEndpointBuilder","display":"Windows Audio Endpoint Builder","state":"Running","path":"C:\\WINDOWS\\System32\\svchost.exe -k LocalSystemNetworkRestricted -p"}]}}
```

## 特殊情况

本机实测 28 条注册表项中 `disabled:true` 为 1 条、`false` 为 18 条、`null` 为 9 条。分别显示“已禁用”“未禁用”“状态未知”，不能把 `null` 写成已启用。`allUsers` 启动文件夹实测 `items:[]`，仍保留文件夹字段组和“无启动项”提示。

`services[]` 查询已限定自动启动；`state` 只是当前运行状态，101 条服务按状态与名称排列，沿用字段组。长命令与路径允许折行；服务 `path` 可能在工具返回前已截断。采集错误时保留 `degraded/collectionErrors/notice` 与已取得的数据。整次 `isError` 使用原文块。

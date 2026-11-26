---
name: driver
description: driver 设备与驱动诊断：异常设备、核心硬件与签名驱动、外设与可移动存储、条件查找设备。
---

# driver

只读设备与驱动状态工具（实时负载归 `sys`，卷容量与 SMART 归 `disk`）。返回结构为 `{ "<scope>": { ... }, "elapsedMs": N }`，`degraded`、`collectionErrors` 与 `notice` 位于 `[scope]` 内。

## 场景选择

| 现象 | 调用 | 结束条件 |
|---|---|---|
| 设备管理器报错或硬件失灵 | `driver({ scope: "problem" })` | 列出异常设备及错误码，或确认无枚举异常 |
| 断网、蓝牙失效、无声或查核心驱动版本 | `driver({ scope: "core" })` | 核对四类硬件、服务状态与签名驱动版本日期 |
| 外接 U 盘、蓝牙外设或外接屏异常 | `driver({ scope: "external" })` | 列出外设与可移动存储，注意可能含内置摄像头与内置屏 |
| 按名称、设备类或硬件 ID 查设备 | `driver({ scope: "find", name?, class?, id? })` | 命中多条时按完整 `deviceId` / `hardwareIds` 核验目标设备 |

拿到 `hardwareIds` 后可用 `web_search` 查官方驱动，驱动卸载或重装命令写入 `runbook` 交队员执行。

## 参数

| 参数 | 适用 `scope` | 约束与默认值 |
|---|---|---|
| `scope` | 全部 | `problem`（默认）、`core`、`external`、`find` |
| `name` | `find` | 设备名称子串，忽略大小写，上限 512 字节 |
| `class` | `find` | 设备类子串（常用英文类名如 `Net`、`MEDIA`、`Bluetooth`、`Display`），上限 512 字节 |
| `id` | `find` | 同时匹配 `deviceId` 与 `hardwareIds` 子串（如 `VID_045E`），上限 512 字节 |

`find` 须至少提供一个去空白后非空的条件，多个条件按 AND 组合。匹配使用传入原串，不用的键直接省略，传参前去除首尾空白。

## 各 scope 返回字段与判读

### problem 与 find

返回 `count`、`devices[]: { name, class, status, errorCode, deviceId, hardwareIds[] }`。

- `problem` 仅筛选 `Status` 为 `Error` 或 `Unknown` 的 PnP 设备；`find` 覆盖全量 PnP 设备。
- `errorCode` 为配置管理器原始错误码，`0` 不能推翻异常 `status` 或单独证明健康。
- 采集无误时 `problem` 为空仅说明未枚举到该状态设备，不能确认整机健康。

### core

| 字段 | 结构与边界 |
|---|---|
| `net[]` | `name`、`connId`、`physical`、`connStatus`。仅保留存在连接名的适配器且包含虚拟网卡，不是完整网卡盘点；`PhysicalAdapter` 缺失时投影为 `false` |
| `bluetooth[]` / `audio[]` | `name`、`status`、`errorCode` |
| `display[]` | `name`、`vendor`、`driver`、`status`、`bus` |
| `services[]` | `name`、`state`（`bthserv` 与 `Audiosrv`；采集无误时缺失才表示无该服务） |
| `drivers[]` | `class`、`device`、`version`、`date`、`provider`（覆盖 NET、BLUETOOTH、MEDIA、DISPLAY 四类签名驱动）。`date` 仅在可解析时归一化为 `YYYY-MM-DD`，其余保留原值或 `null` |

### external

返回 `devices[]: { name, class, status, errorCode, deviceId }`（不含 `hardwareIds`，按 `USB`、`BTHENUM`、`DISPLAY` 前缀筛选）与 `removable[]: { model, interface, mediaType, sizeGB }`。

## 错误与超时规则

1. 超时上限 30 秒，超时或全部采集源失败返回 `isError: true`（`{"error", "elapsedMs"}`）。
2. 部分子查询失败或原生回退保留在 `[scope].degraded: true` 与 `[scope].collectionErrors` 中，按错误 `source` 判断哪组数据受影响；采集失败后的空数组不可解释为无异常或无设备。

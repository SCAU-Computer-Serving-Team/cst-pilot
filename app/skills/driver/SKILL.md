---
name: driver
description: driver 设备与驱动诊断参考。排查异常设备（problem）、核心硬件与签名驱动版本（core）、外设与可移动存储（external）或按条件查设备（find）时查阅。
---

# driver

只读设备与驱动健康工具，普通权限可用。职责分工：`driver` 查设备识别与驱动状态，`sys` 查实时负载，`disk` 查卷容量与 SMART。

## 排查路径

1. 硬件失灵或设备管理器报错先查 `problem`。
2. 断网、蓝牙失效、无声音或显卡驱动异常查 `core`。
3. 外接 U 盘、移动硬盘、蓝牙外设或外接屏异常查 `external`。
4. 已知设备名、设备类或硬件 ID 时用 `find` 精确定位；拿到 `hardwareIds` 后可用 `web_search` 核查官方驱动，驱动卸载或重装命令写入 `runbook` 交队员执行。
5. 核对顶层 `elapsedMs`、`degraded`、`collectionErrors` 与 `notice`，仅在采集无错误时将空数组解读为未枚举到目标设备。

## 参数

| 参数 | 适用 `scope` | 约束与默认值 |
|---|---|---|
| `scope` | 全部 | `problem`（默认）、`core`、`external`、`find` |
| `name` | `find` | 设备名称子串（忽略大小写，上限 512 字节） |
| `class` | `find` | 设备类子串（常用英文类名如 `Net`、`MEDIA`、`Bluetooth`、`Display`，上限 512 字节） |
| `id` | `find` | 设备 ID 或硬件 ID 子串（如 `VID_045E`，双通道匹配，上限 512 字节） |

`find` 至少提供一个条件，多个条件按 AND 组合。

## 各 scope 返回字段与判读

返回结构为 `{ "<scope>": { ... }, "elapsedMs": N }`。

| `scope` | 返回字段 | 判读要点 |
|---|---|---|
| `problem` | `count`、`devices[]` | 仅筛选 `Status` 为 `Error` 或 `Unknown` 的 PnP 设备。每项含 `name`、`class`、`status`、`errorCode`（系统原始错误码，0 为正常）、`deviceId`、`hardwareIds[]`。采集无误时为空仅说明未枚举到该类异常，不能单独确认整机健康 |
| `core` | `net[]`、`bluetooth[]`、`audio[]`、`display[]`、`services[]`、`drivers[]` | `net` 含 `name`、`connId`、`physical`、`connStatus`（含虚拟网卡，`physical` 偶发误报仅作展示）；`bluetooth` 与 `audio` 含 `name`、`status`、`errorCode`；`display` 含 `name`、`vendor`、`driver`、`status`、`bus`；`services` 为 `bthserv` 与 `Audiosrv` 状态（采集无误时缺失才表示无该服务）；`drivers` 覆盖 NET、BLUETOOTH、MEDIA、DISPLAY 四类签名驱动（`class`、`device`、`version`、`date` 为 `YYYY-MM-DD`、`provider`） |
| `external` | `devices[]`、`removable[]` | `devices` 按 `USB`、`BTHENUM`、`DISPLAY` 前缀筛选（含 `name`、`class`、`status`、`errorCode`、`deviceId`，不含 `hardwareIds`，可能包含内置摄像头与内置屏）；`removable` 为可移动磁盘（`model`、`interface`、`mediaType`、`sizeGB`） |
| `find` | `count`、`devices[]` | 字段与 `problem` 相同，覆盖全量 PnP 设备 |

## 通用边界与耗时判读

1. 设备名称随驱动可能为任意语言，原样透传不做语言过滤。
2. 超时上限 30 秒；`elapsedMs` 偏高提示设备树或驱动枚举阻塞。
3. 全部采集源失败返回 `isError: true`（`{"error", "elapsedMs"}`）；部分失败置 `degraded: true` 并附 `collectionErrors`，此时空数组不能当作无异常或无设备。

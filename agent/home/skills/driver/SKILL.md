---
name: driver
description: driver 工具的参数与返回字段说明。四个 scope 的查询范围与字段定义、错误码原样透传约定、ID 白名单前缀、已知盲区、与 sys / disk 的边界。
---

# driver 工具说明

只读设备与驱动健康工具，按 `scope` 选择子功能，不传默认 `problem`。可在普通权限下调用，采集失败时保留错误。与 sys 的边界：sys 管设备负载，driver 管设备健康（在不在、驱动对不对）；与 disk 的边界：盘上容量归 disk。

## 参数

1. `scope`（可选）：`problem` / `core` / `external` / `find`，不传默认 `problem`
2. find 分支参数（至少传一个，AND 组合）：`name`（名称子串）/ `class`（设备类精确名，固定英文类名如 Net、MEDIA、Bluetooth）/ `id`（硬件 ID 或 DeviceID 子串，如 VID_045E，对 deviceId 与 hardwareIds 双通道匹配）

## 返回字段

1. `problem` / `find` 返回 `devices[]`（`name` / `class` / `status` / `errorCode` / `deviceId` / `hardwareIds[]`）与 `count`。`errorCode` 是 ConfigManagerErrorCode 原始值（0 = 正常），不翻译；`hardwareIds` 含 VEN/DEV，供联网定位驱动
2. `problem` 只收 Status 为 Error / Unknown 的 PnP 设备。查询成功时空数组表示未枚举到该范围内的异常，不能据此确认整机健康
3. `core` 返回 `net[]`（`name` / `connId` / `physical` 布尔 / `connStatus` 原始值）、`bluetooth[]` 与 `audio[]`（PnP 设备级）、`display[]`（`name` / `vendor` / `driver` / `status` / `bus`）、`services[]`（bthserv / Audiosrv 的 `state`，仅采集成功时缺失才可解释为无该服务）、`drivers[]`（`class` / `device` / `version` / `date` yyyy-MM-dd / `provider`）
4. `external` 返回 `devices[]`（同 problem 字段行）与 `removable[]`（`model` / `interface` / `mediaType` / `sizeGB`）
5. `physical` 标志可能误报（个别虚拟网卡也返回 true），只作展示

## scope 查询范围

1. `problem`：Status 为 Error / Unknown 的全部 PnP 设备
2. `core`：Net（网络连接面板真实条目，虚拟网卡不剔除）/ Bluetooth / Audio / 显示四类硬件现状 + 两个关键服务状态 + 驱动版本日期表
3. `external`：DeviceID 前缀白名单 `USB\`（连带 `USBSTOR\`）/ `BTHENUM\`（蓝牙外设）/ `DISPLAY\`（显示器）+ 可移动存储（U盘 / 移动硬盘 / SD 卡）
4. `find`：全量 PnP 设备按条件定位

## 通用约定

1. `notice` 是错误码语义、盲区附注与字段导读，转达给队员时不能省略
2. `error` 表示查询失败；`degraded` / `collectionErrors` 表示部分采集失败。只有采集成功时，空数组才可解释为未枚举到对应设备
3. 设备名随驱动可能是任意语言，只透传不过滤

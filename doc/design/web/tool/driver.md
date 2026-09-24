# driver 工具卡片

[driver.ts](../../../../agent/home/extensions/diagnostics/driver.ts) 返回 `details[scope]`，默认 `problem`。2026-09-23 逐 scope 调用项目扩展；以下 JSON 为实测结果节选，设备名称保留原值，唯一设备标识局部遮盖。

## 主模板与字段组

四种 scope 均以**概览组**为主模板。设备按来源重复字段组，排序不代表风险大小。

| scope | 字段组 | 展示字段 |
|---|---|---|
| `problem` | 异常设备、汇总 | `count`；`devices[].name/class/status/errorCode/deviceId/hardwareIds` |
| `core` | 网络、蓝牙、音频、显示、服务、驱动 | `net[]/bluetooth[]/audio[]/display[]/services[]/drivers[]`，各按原始字段生成行 |
| `external` | 外设、可移动存储 | `devices[]` 与 `removable[]` 分组，保留设备状态及大小 |
| `find` | 匹配设备、汇总 | `devices[]` 与 `count`；查询条件取调用参数 `name/class/id` |

## 各 scope 的实测返回

### problem

```json
{"problem":{"devices":[],"count":0}}
```

### core

实测六类字段均有返回：

```json
{"core":{"net":[{"name":"Realtek 8832CU Wireless LAN WiFi 6 USB NIC","connId":"WLAN","physical":true,"connStatus":7}],"bluetooth":[{"name":"Generic Bluetooth Adapter","status":"OK","errorCode":0}],"audio":[{"name":"NVIDIA High Definition Audio","status":"OK","errorCode":0}],"display":[{"name":"GameViewer Virtual Display Adapter","vendor":"GameViewer","driver":"15.6.5.199","status":"OK","bus":"ROOT"}],"services":[{"name":"Audiosrv","state":"Running"}],"drivers":[{"class":"NET","device":"Hyper-V Virtual Switch Extension Adapter","version":"10.0.26100.1","date":"2006-06-21","provider":"Microsoft"}]}}
```

实测六个数组长度依次为 `10/17/8/2/2/49`；上方每类只列首项。

### external

```json
{"external":{"devices":[{"name":"BESOTA","class":null,"status":"OK","errorCode":0,"deviceId":"BTHENUM\\{66666666-6666-6666-6666-666666666666}_VID&000102B0_PID&0000\\7&xxxxxxx&0&0812xxxxxxxx_C00000000"}],"removable":[]}}
```

实测 `devices.length:49`、`removable.length:0`；示例只列首个设备。

### find

输入 `name:"不存在的诊断样本设备"`，实际返回：

```json
{"find":{"devices":[],"count":0}}
```

## 特殊情况

`problem` 与 `find` 的空列表来自完成的枚举，分别表示本次未发现异常设备和未匹配条件；`external` 的 `removable:[]` 与 49 条 `devices` 并存，卡片保留两组。`find` 不传任何条件时实测整次抛错：`find 需要至少一个条件：name（名称子串）/ class（设备类精确名）/ id（硬件 ID 子串）`，使用原文块。

采集结果含 `degraded/collectionErrors` 时仍展示已返回设备，并附失败提示。`errorCode` 是原始错误码，卡片不自动判断故障原因；该降级分支由代码确认，本次未触发。

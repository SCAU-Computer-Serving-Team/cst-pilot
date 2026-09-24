# eventlog 工具卡片

[eventlog.ts](../../../../agent/home/extensions/diagnostics/eventlog.ts) 返回 `details[scope]`，默认 `recent`。2026-09-23 用项目扩展逐 scope 实测；下方 JSON 为返回字段节选，数组只列首项；主机名局部遮盖。

## 主模板与字段组

八种 scope 均以**概览组**为主模板。`recent/boot/crash/service/disk/security/query` 的 `events[]` 按时间倒序生成事件字段组：`time/logName/provider/id/levelName/recordId/msg`。`counts[]` 按次数 `n` 追加来源排行组，`total/hours/top/truncated/unreadable` 为范围字段组。`detail` 用事件字段组与 `msg` 原文块。

## 各 scope 的实测返回

### recent

输入 `hours:24,top:2`，实际返回 `total:12315`、2 条事件、4 条次数记录：

```json
{"recent":{"total":12315,"truncated":true,"events":[{"logName":"System","time":"2026-09-23 18:37:48","recordId":390246899,"level":3,"levelName":"Warning","provider":"Microsoft-Windows-WHEA-Logger","id":17}],"counts":[{"provider":"Microsoft-Windows-WHEA-Logger","id":17,"n":12301,"last":"2026-09-23 18:37"}]}}
```

### boot

同一时间窗，实际返回 2 条事件、1 条次数记录：

```json
{"boot":{"total":12299,"truncated":true,"events":[{"logName":"System","time":"2026-09-23 18:37:53","recordId":390247092,"provider":"Microsoft-Windows-WHEA-Logger","id":17}],"counts":[{"n":12299}]}}
```

### crash

同一时间窗，实际返回 2 条事件、1 条次数记录：

```json
{"crash":{"total":12,"truncated":true,"events":[{"logName":"Application","time":"2026-09-23 02:04:57","recordId":179173,"levelName":"Information","provider":"Windows Error Reporting","id":1001}],"counts":[{"n":12}]}}
```

### service

```json
{"service":{"total":0,"truncated":false,"events":[],"counts":[]}}
```

### disk

```json
{"disk":{"total":0,"truncated":false,"events":[],"counts":[]}}
```

### security

普通权限下无 `total/events/counts`；实际结果见[特殊情况](#特殊情况)。

### query

输入 `logName:"System",ids:[41],hours:24,top:2`：

```json
{"query":{"total":0,"truncated":false,"events":[],"counts":[]}}
```

### detail

用 `logName:"System",recordId:390246899` 读取 `recent` 的首条事件：

```json
{"detail":{"found":true,"logName":"System","recordId":390246899,"time":"2026-09-23 18:37:48","levelName":"Warning","provider":"Microsoft-Windows-WHEA-Logger","id":17,"machine":"Timxxxx-PC","msg":"发生了已更正的硬件错误。 \r\n\r\n组件: PCI Express Root Port\r\n错误源: Advanced Error Reporting (PCI Express)\r\n\r\n主总线: 设备: 函数: 0x0: 0x1: 0x0\r\n辅助总线: 设备: 函数: 0x0: 0x0: 0x0\r\n主要设备名称: PCI\\VEN_8086&DEV_460D&SUBSYS_00000000&REV_02\r\n辅助设备名称: "}}
```

`msg` 使用原文块，保留换行。

## 特殊情况

`security` 的真实返回为 `{"security":{"admin":false,"degraded":true,"type":"all","notice":"security 需要管理员权限：当前以非管理员运行，Security 日志不可读，未执行查询。以管理员身份重启 pi 后可用（type=all / logonFail / lockout）。"}}`。没有事件数；卡片显示权限提示，不能解释为 0 条安全事件。

`detail` 按 `logName:"System",id:41` 的真实返回为 `{"detail":{"found":false,"notice":"未找到该记录（logName=System id=41）；记录可能已被滚动清除或不在此通道。"}}`。保留未找到提示；上方成功分支则绘制事件字段组和完整消息。`recent` 的 `total:12315`、`events.length:2`、`truncated:true` 表明总数与当前页不同，截断提示须保留。整次 `isError` 使用原文块。

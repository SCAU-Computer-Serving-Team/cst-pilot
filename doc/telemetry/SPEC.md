# 使用情况遥测规格

`telemetry` 模块在 Pi 进程内采集会话级聚合统计与本地请求日志，并将会话快照同时上传至 CSTOA 与 `timserver_1` 两个独立接收端。字段定义与隐私边界以 [../contract.md](../contract.md) 为准，内部架构见 [ARCHITECTURE.md](ARCHITECTURE.md)，协议与接口见 [API.md](API.md)，部署运维见 [OPERATIONS.md](OPERATIONS.md)。

## 启停与运行条件

| 条件 | 行为要求 |
|---|---|
| `telemetry.json` 中 `enabled=true` | 订阅会话事件累计指标；在回答结束、定时心跳、正常关闭与启动恢复时持久化快照并入队上传 |
| `enabled=false` | 完全静默：不订阅事件、不拦截 CSTOA 请求、不查账单、不初始化安装身份、不探测系统、不写草稿或队列、不发心跳或上传 |
| 存在有效 CSTOA OAuth 令牌 | 优先以 Bearer 令牌上传（`0.2` 同时附安装签名），服务端通过 OA 内省填充上传者 `mid` 与 `deviceId` |
| 未登录、令牌过期或被拒（401/403） | 自动使用本地安装密钥（Ed25519）签名上传；两端首次验签通过后永久绑定该 `installationId` 的公钥 |
| 网络故障或接收端 429/5xx | 快照保留在各端独立持久队列（每端上限 200 条）中，后续定时检查、唤醒或重启后自动补发；过期纯心跳直接丢弃 |

## 多触发提交与心跳在线规则

1. **统一提交入口**：TUI 与 Web 的每个会话实例独立累计。中间轮次（`turn_end` 与 CSTOA HTTP 尝试）实时写入本地恢复草稿；当触发**回答结束（`agent_settled`）**、**30 秒定时心跳检测到变化**、**实例正常关闭**或**启动恢复残留草稿**时，冻结当前统计并生成递增 `revision` 的 `0.2` 快照写入双端队列。
2. **实例持续累计**：回答结束提交快照后，会话实例保持活跃，后续提问在同一 `recordId` 上继续累加并递增 `revision`；提问数、轮次数与请求尝试数全为 0 的空实例在关闭时不产生遥测记录。
3. **进程级心跳与在线判定**：每个 Pi 进程维护单一 `runId` 与递增心跳序号 `sequence`，默认每 30 秒向两端发送在线状态（`idle` / `busy` / `closed`）。接收端按服务端接收时间判定，90 秒未收到有效更新标记为失联；失联或纯心跳绝不增加会话数、模型用量或活动时长，历史快照补发也不恢复旧进程的在线状态。
4. **正常关闭预算**：程序真实退出时等待本地草稿落盘、有限账单查询与最终快照入队（同一 home 共享 5 秒总截止预算），不等待远端上传确认；进程强杀、断电或拔盘留下的未关闭草稿在下次启动时按 `endReason="crash"`（截止时间取最后已保存活动时刻）恢复入队。

## 统计口径与保留期

| 维度 | 统计规则 |
|---|---|
| 会话时间与原因 | `startedAt` 至本次快照截止 `endedAt` 为 `durationMs`；各轮 `turn_start` 至 `turn_end` 累加为 `activeMs`。开始原因取 `startup/new/resume/fork/reload`，结束原因取 `quit/new/resume/fork/reload/delete/crash`（活跃快照 `endReason=""`） |
| 身份与环境固定 | `identity`、`os`、`runtime`、`admin` 在会话启动时快照固定；U 盘换机补发保持原机器环境不变 |
| 模型用量 | 按 `provider + model + thinkingLevel` 聚合（最多 32 项）；缺少 usage、`totalTokens=0` 或以 `error/aborted` 结束的轮次计入 `incompleteUsageTurns` |
| CSTOA 扣点 | `cstoaBilling` 按网关响应头 `x-oneapi-request-id` 向 OA 账单接口查询本人请求扣点，成对记录观察请求数 `requests`、已确认数 `settledRequests` 与确认总额 `quotaPoints`；没有任何已确认账单时 `quotaPoints=null` |
| 上下文与工具 | 记录压缩次数、压缩前总 token、溢出触发次数、压缩失败数、上下文峰值与对应窗口；工具按 `name + scope` 聚合（最多 512 项），同次调用每项指标至多计 1 次 |
| 错误与报错原文 | 统计非 2xx 状态码分布 `providerErrors`、无响应 `networkErrors` 与 `aborted`；按原文分组降序保留最多 10 组 `errors[].{message, count}`，单条记录超 1 MiB 不入队 |
| 服务端保留期 | 会话统计、身份与每日汇总长期保留；`sessions.payload.errors` 满 180 天清除原文；上传日志 `uploads` 与运行在线状态 `process_runs` 满 90 天删除（数据库备份同步遵循该边界） |

## 验收条件

- 同一会话连续多轮提问触发多次 `0.2` 快照上传后，双端数据库中会话实例数保持为 1，累计 token、工具调用与账单扣点准确替换为最新 `revision`。
- 模拟一端超时或断网时另一端正常入库确认，恢复后待发队列精确补发且旧版本 `ack` 不误删发送期间产生的新版本。
- 进程强杀后重启可恢复残留草稿并标为 `crash`；跨机或 PID 碰撞时不误跳过已终止实例，也不覆盖本机正在运行的实例。

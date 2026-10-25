# 发送端规格

发送端在 Pi 内采集会话统计，同一记录写入各接收端的独立队列。协议版本为 `0.1`，字段见[信息收集契约](../../contract.md)和[会话记录](../schema.md)，验证方法见[遥测测试](../../test/telemetry.md)。

## 采集边界

1. TUI 与 Web 使用相同统计规则，每个会话实例独立累计。
2. 事件采集不等待网络请求，不保存对话和工具正文。CSTOA 账单查询只读本地对应凭据，不上传凭据；模型请求报错原文按契约保存。
3. 会话启动时异步初始化安装身份并探测系统，草稿和定稿等待该实例的结果。探测失败的属性为 `null`，补发不重新探测。
4. 安装标识及密钥只写 `agent/home/telemetry/identity.json`，多进程初始化共用同一文件锁。已有文件损坏时不重建；首次无法持久化时身份保持未知。
5. Windows 只读取系统名称、功能版本、原生架构、PE、权限及 MachineGuid。原始 MachineGuid 仅在内存中参与 HMAC，PE 或来源不可靠时不生成环境标识。
6. `cost` 与 `currency` 同源，时间使用带时区偏移的 ISO 8601，工具包版本读取失败为空字符串。

## 事件与统计

| 事件 | 统计 |
|---|---|
| `session_start` | ID、开始原因、时间、通道及独立环境快照；上一实例未定稿时先按 crash 定稿 |
| `input` | `interactive` 或 `rpc` 来源的提问次数 |
| `before_provider_request` | 请求开始时的思考强度 |
| `turn_start` / `turn_end` | 活动时长、轮次、模型用量、费用、上下文峰值、中断与错误；结束后保存草稿 |
| `after_provider_response` | 非 2xx 响应的状态码计数；轮次出错且未收到响应时计网络错误 |
| `tool_call` | 工具名与 scope，省略时使用工具默认 scope |
| `tool_execution_start` / `tool_execution_end` | 配对累计次数、失败、耗时、结果字节数、降级与裁剪 |
| `session_compact` / `session_compact_failed` | 压缩次数、压缩前 token、溢出原因及失败次数 |
| CSTOA 推理 HTTP 开始／响应 | 记录请求尝试与网关请求 ID，异步保存草稿；不读取响应正文 |
| 实例结束 | 固定结束时刻、原因与上下文条目数；有限补查账单，定稿后入队 |

同一次工具调用的每项指标最多计一次。

| 指标 | 来源 |
|---|---|
| 降级 | `sys`、`driver`、`eventlog` 的 `details[scope].degraded`；startup、bitlocker 对应包装的标记；disk 顶层标记、`usage.method=node-walk` 或非空 `usage.degradedFrom` |
| 裁剪 | 诊断结果的 `outputTruncated`；内置 bash 的 `details.truncation.truncated`，缺少标记时不推测 |
| 命令条数 | `runbook.items` |
| 搜索服务与抓取方式 | web_search 的 providers、fetch_content 的 modes |

## 会话与草稿

| 场景 | 规则 |
|---|---|
| 请求与轮次 | HTTP 开始／响应及轮次结束均触发草稿；轮次结束异步查账后写入，保存最后完成轮次时刻；写入串行执行 |
| TUI 结束 | `session_shutdown` 定稿 |
| Web 结束 | 删除会话或关闭运行层定稿；切换页面不结束实例 |
| 定稿 | 提问和轮次均为零时不产记录；其余先保存定稿草稿，再写全部队列，成功后删草稿 |
| 启动恢复 | 跳过其他仍存活进程的草稿；未完成实例按 crash 恢复，结束取最后完成轮次；已定稿草稿保留原结束原因与时刻 |
| 恢复失败 | 入队失败保留源草稿；重试沿用 recordId，全部目标入队后才删除 |
| 正常退出 | 等待草稿链、有限账单查询及定稿落盘，不等待上传确认；已有上传任务可能被进程退出中断 |
| 强杀 | 依赖最后已写草稿；可恢复已落盘的请求尝试，未完成轮次的 usage 可能缺失 |

| 通道 | 开始原因 | 结束原因 |
|---|---|---|
| TUI | startup、new、resume、fork、reload | quit、new、resume、fork、reload、crash |
| Web | new、resume、fork | quit、delete、crash |

## CSTOA 账单查询

`oauth/request-observer.ts` 仅在启用遥测且订阅当前 sessionId 时包装 CSTOA 推理 fetch。每次实际 HTTP 尝试分别记录；从 `x-oneapi-request-id` 取请求 ID，不改变推理正文或等待查询。

1. `billing.ts` 只读 auth.json 中有效 CSTOA OAuth 访问令牌或 APIKEY，向固定 OA 地址 `POST /api/agent/llm/billing` 查询，每批最多 50 个 ID。安装签名不能查账，不刷新或写回凭据。
2. 每次查询超时 2 秒，禁止重定向。轮次结束在后台草稿链中查询；定稿先等草稿链，再在 4.5 秒总限内查询，未全确认时隔 300ms 再查一次。会话结束时间在等待前固定。
3. 启动恢复可对未定稿草稿补查一次，使用 4.5 秒总限；已定稿草稿和待发记录保持原内容。缺少有效 ID 时不发查询。
4. 查询失败保留未知，不阻塞推理、不输出凭据。请求 ID 仅存在本地草稿；上传对象只含[扣点汇总](../schema.md#cstoa-扣点)。两端收到同一份定稿。

## 队列与发送

每个端点使用独立 JSONL 队列，上限 200 条，按 recordId 去重，超限淘汰最旧记录。追加、确认删除与身份初始化使用目录锁，兼容 FAT/exFAT；锁内不执行网络操作，失效进程锁可回收。

1. 启动、草稿恢复、定稿后触发发送；进程存活期间每 30 秒重读凭据并重试，恢复网络或登录后最多等待下一轮。定时器不阻止退出。
2. 同一进程、home、端点只运行一个发送任务，其他触发合并。不同进程可能重发，服务端去重。
3. 原 pending.jsonl 先复制至所有目标，再清除源记录；端点移除后保留其队列。
4. 每批最多 50 条、记录正文合计不超过 900 KiB。单条大于 1 MiB 不入队，超出发送预算的记录整条移除，不裁剪报错文本。
5. 写入失败保留已有文件，容量淘汰、未落盘轮次和强杀仍有数据丢失边界。

| 结果 | 行为 |
|---|---|
| 202 | 清除该端点本批；逐条拒收也在确认范围内，不再重发 |
| 401 / 403 | 保留队列；OA 凭据被拒时尝试安装签名 |
| 400 / 413 | 丢弃该批 |
| 429 / 5xx / 网络错误 / 超时 | 保留队列，等待后续触发或 30 秒重试 |
| 404 / 410 | 本进程停止该端点发送，保留队列 |
| CA 缺失或 TLS 错误 | 保留队列，不降低证书验证 |

单次网络超时 5 秒，不跟随重定向。采集和发送失败不影响诊断，不输出凭据。

## 凭据与配置

有效 OA 凭据优先，只读 auth.json，不刷新或写回；缺少、过期或被拒时使用安装签名。mid 和 deviceId 仅表示本次上传者，不能回填为历史会话操作人。

安装签名采用 Ed25519。32 字节种子由 `HMAC-SHA256(identityKey, UTF-8("upload-signing"))` 派生，与环境标识的用途分离；服务端首次收到有效签名时绑定安装与公钥，不接收密钥原文。

| 鉴权头 | 内容 |
|---|---|
| `Authorization` | `Bearer <OA访问令牌>` 或 `Installation <installationId>` |
| `X-Telemetry-Key` | 安装公钥的 DER/SPKI，base64url，无填充 |
| `X-Telemetry-Time` | Unix 毫秒字符串，接收端允许前后 10 分钟 |
| `X-Telemetry-Signature` | 对签名原文的 Ed25519 签名，base64url，无填充 |

签名原文为以下四项用换行连接，不加末尾换行：`telemetry-upload`、installationId、时间头原文、请求正文原始 UTF-8 字节的 SHA-256 小写十六进制。后 3 个头仅安装签名使用；重试重新签名，记录内容与 recordId 保持。

安装签名只证明密钥持有权，不证明自然人或物理电脑真实性。系统时钟偏差超过窗口时保留队列，校时后重试。

`agent/home/telemetry.json`：

```json
{
  "enabled": true,
  "endpoints": [
    { "url": "https://www.cstoa.top/api/telemetry" },
    { "url": "https://8.163.28.9:8445/api/telemetry", "caFile": "timserver_1.crt" }
  ],
  "authProvider": "cstoa"
}
```

配置启动时加载，修改后重启。enabled=false 不采集、不订阅 CSTOA 请求、不查询账单、不初始化身份、不探测系统、不写队列、不上传。公网端点必须 HTTPS，回环测试允许 HTTP；拒绝 URL 中的用户名、密码与片段。

caFile 仅用于对应端点，继续校验证书链、有效期与主机名。币种见[契约](../../contract.md#计费币种)。发行配置由 pack/release-settings.mjs 维护，白名单装配排除 telemetry/ 运行目录；Pi 自带遥测另由 PI_OFFLINE 与 enableInstallTelemetry=false 关闭。

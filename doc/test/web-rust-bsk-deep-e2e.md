# Rust Web 深度遍历端到端测试设计（bsk）

本文定义基于单窗口 `bsk`（Browser Skill）、`cst-server --standalone-web` 与真实模型 `opencode-go/deepseek-v4.1-flash` 的深度遍历端到端测试方案。[web-rust-bsk-e2e.md](web-rust-bsk-e2e.md) 覆盖 14 个路由与 28 个静态页内分支的广度遍历。本文聚焦长生命周期会话在真实多轮工具调用、500K 超长上下文以及显式与隐式状态机交叉转移下的端到端行为。产品规格见 [web/SPEC.md](../web/SPEC.md)，接口定义见 [web/API.md](../web/API.md)，阶段设计见 [8.web.md](../rust/plan/8.web.md)。

## 1. 对话状态机与深度遍历拓扑

深度遍历连接前端收发箱与后端双层模型循环，并覆盖真实模型推理、挂起交互桥接和多路复用事件流：

```mermaid
stateDiagram-v2
    [*] --> Idle: 打开或新建会话

    state "前端乐观发送 (MessageOutbox)" as Outbox {
        Waiting --> Sending: 轮到队首提交
        Sending --> Accepted: 202 Accepted 写入 web-inbox
        Sending --> Failed: 4xx / 5xx 明确拒绝
        Sending --> Uncertain: 202 响应丢失或宿主重启遗留 delivering
    }

    Idle --> Outbox: 用户发送消息
    Accepted --> InboxPending: SessionInbox 落盘

    state "后端收件箱与双层模型循环 (SessionInbox × AgentLoop)" as Loop {
        InboxPending --> Prompting: 读取附件图片 (prompting 锁)
        Prompting --> Streaming: 经本机透明代理请求 opencode-go/deepseek-v4.1-flash
        Streaming --> RetryBackoff: 429 / 503 写入 ContextEditEntry 退避重发
        RetryBackoff --> Streaming: 隐藏失败半轮重调真实模型
        Streaming --> OverflowCompact: 超过 500K 窗口报错 (单次自动压缩)
        OverflowCompact --> Streaming: 基于真实压缩摘要重发原提示词
        Streaming --> TruncatedToolErr: StopReason::Length 跳过工具并生成错误 toolResult
        TruncatedToolErr --> Streaming: 下一内层轮次补发完整参数或触发单次恢复
        Streaming --> ToolBatch: toolUse 参数强转与校验通过后执行当批工具
        ToolBatch --> UIBridge: bitlocker 或 ui_requests 挂起等待
        UIBridge --> ToolBatch: 200 单次原子消费 (过期或重复返回 409)
        ToolBatch --> NextTurnPrep: 当批工具完成并执行 finish_turn
        NextTurnPrep --> ThresholdCompact: 下一内层轮次前投影估算跨过 483,616 tokens
        NextTurnPrep --> SteerInject: 提取 steer 插话优先于普通排队项进入下一内层轮次
        SteerInject --> Streaming: 注入插话调用真实模型
        NextTurnPrep --> Streaming: 无插话时回填当批 toolResult 继续内层循环
        Streaming --> ThresholdCompact: 回合结束 Stop 且真实 usage 跨过 483,616 tokens
        ThresholdCompact --> OuterFollowUp: 调用真实模型生成摘要并追加 CompactionEntry
        Streaming --> OuterFollowUp: Stop 且无后续工具调用
        OuterFollowUp --> Streaming: 排空 follow-up 队列继续外层循环
        Streaming --> AbortedPaused: POST /abort 取消流与退避等待并设 paused=true
    }

    OuterFollowUp --> SnapshotCommit: 流式节拍排空 + 防抖快照对齐
    AbortedPaused --> Idle: 用户恢复发送或编辑重排队列
    SnapshotCommit --> Idle: 会话回到空闲态
```

下表列出本方案覆盖的 5 类显式状态机与 5 类隐式状态机：

| 类别 | 状态机名称 | 核心实现模块 | 关键状态与转移边界 | 对应套件 |
|---|---|---|---|---|
| 显式 | 前端发送箱 `MessageOutbox` | `src/web/frontend/app/data/message-outbox.ts` | `waiting -> sending -> accepted / failed / unknown`。支持 `202` 丢失后按原 ID 对账与幂等重试 | D3 与 D4 |
| 显式 | 后端收件箱 `SessionInbox` | `crates/cst-server/src/session/inbox.rs` | `pending -> delivering -> delivered / failed / cancelled`。支持 `direct / queue / steer` 三态、`paused` 暂停锁与 `version` 乐观锁（`409`） | D3 与 D4 |
| 显式 | 双层循环与压缩 `AgentLoop` | `crates/pi-agent/src/agent_loop.rs` 与 `compaction.rs` | 外层续答与内层工具批次循环。`Length` 生成未执行报错 `toolResult` 继续收敛。覆盖轮末真实用量与轮前投影估算两类 `Threshold`、`Split-Turn` 双摘要及 `Overflow` 单次恢复 | D1 至 D4 |
| 显式 | 挂起交互桥 `BitLocker / UI` | `crates/cst-server/src/session/interactions.rs` | `Pending`（180s 期限，`revision` 递增）-> `400` 保持挂起 -> `200` 单次消费 -> 新幂等键重复提交或超时返回 `409` | D4 |
| 显式 | 聚合事件流 `SharedWorker SSE` | `event-stream-worker.ts` 与 `session/events.rs` | `/api/state` 协商 -> `stream_ready` -> 单飞订阅（每分片 32 会话）-> `global:<e0>:<s0>,<sid>:<e1>:<s1>` 256 帧重放与定向 `event: reset` | D6 |
| 隐式 | 流式渲染与快照交接 | `web-state.ts` 与 `session-stream.ts` | 流式阶段正文单调追加不倒退。`message_end` 原位承接消息，防抖 HTTP 快照替换期间正文持续非空且定稿与 JSONL 一致 | D1 |
| 隐式 | 单轮交替分段与懒挂载 | `turn-segments.ts` 与 `assistant-content.tsx` | 普通工具归入 `process` 折叠段，4 类常驻组件归入 `visible` 独立段。历史折叠段首次展开才挂载 DOM | D1 |
| 隐式 | 附图加载窗口与队列并发 | `crates/cst-server/src/session/inbox.rs` | `load_images` 异步读盘期间持有 `prompting` 标记，防止并发 `steer` 提前启动空轮次 | D3 |
| 隐式 | 双视图投影与空闲池淘汰 | `crates/cst-server/src/session/slot.rs` 与 `mod.rs` | `ContextEditEntry` 裁剪模型请求中的失败半轮而 UI 分支保留记录。`max_idle_sessions = 10` 仅统计 `!is_pinned` 空闲会话，保护 5 类活跃会话 | D2 至 D5 |
| 隐式 | 长列表滑窗与导出崩溃对账 | `conversation.tsx` 与 `exports/pagination.rs` | 80 条滑窗分页与 `100px` 离底锚点。导出限制 `100,000px` 与 `32 MiB`（`413`），支持真实强杀与静态夹具重启对账 | D2 与 D4 |

## 2. 测试环境、500K 校准与执行约定

### 2.1 隔离双宿主与真实模型接入

1. **主宿主（`kitRoot-main`）**：在 `E:\tmp\<YYYY-MM-DD>\deep-e2e\kit-main\` 下创建独立目录，写入 `APPEND_SYSTEM.md` 与 `skills/`，软链接 `agent/home/extensions/web/static/`，启动 `target\debug\cst-server.exe --standalone-web`。仅从本机配置提取 `opencode-go` 凭据写入隔离目录 `agent/home/auth.json`，不修改本机真实认证文件或会话记录，测试日志与报告全程脱敏。
2. **辅助宿主（`kitRoot-aux`）**：针对无需本机真实管理员权限与加密卷的挂起桥接与空闲池核验（`D3-5` 附图锁及 `D4-1`、`D4-2`、`D5-4`），在独立目录 `kit-aux\` 启动 `cst_server::RunningServer` 集成宿主（挂载模拟 `BitLockerSource` 与受控延迟图片存储，并暴露 `ServerUiBridge` 触发端点），避免与主宿主争用 `.state/server.json` 单实例锁。
3. **500K 窗口覆写与网关请求头**：`settings.json` 默认模型设为 `opencode-go/deepseek-v4.1-flash`。`models.json` 通过 `modelOverrides` 将 `deepseek-v4.1-flash` 的 `contextWindow` 覆写为 `500000`（配合 `reserveTokens = 16384`，自动压缩阈值为 `483,616` tokens），保留原生思考推理、图片输入与 `384,000` 最大输出配置，并注入 `x-opencode-session` 请求头以满足 OpenCode Go 路由校验并复用服务端前缀缓存。
4. **作用域受控的透明转发代理**：在 `127.0.0.1` 启动本机代理并将 `opencode-go` 的 `baseUrl` 指向该代理。常规对话与工具链，以及压缩和分支总结请求，均原样透传至 `https://opencode.ai/inference/go/openai/v1` 真实推理，代理旁路记录完整上游请求体供上下文裁剪与分支隔离断言核验。故障注入与流式暂停门控严格绑定 `(sessionId, requestKind, attempt)` 三元组，防止误伤并发压缩或分支总结请求。

### 2.2 500K 上下文校准与请求预算

1. **预置长历史校准**：预置 JSONL 历史的字符估算（`chars / 4`）与上游真实分词存在偏差，不采用伪造 JSONL `usage` 字段充当证据。测试先构造高密度多轮排障历史 JSONL，发出 1 次真实探针请求读取上游返回的真实 `usage.prompt_tokens`，将基线校准在 `[445,000, 465,000]` 区间（距 `483,616` 阈值留出 `18,000–38,000` tokens 余量）。验收表述统一记为“预置长历史基线后，由真实模型处理近 500K 上下文并跨过阈值”。
2. **受截断约束的分块补读**：`read` 工具默认单次上限为 `2000 行 / 50 KiB`（`crates/pi-runtime/src/tools/truncate.rs:8`）。工作区预置 6 份 `48 KiB` 独立编号日志分块（`chunk-01.log` 至 `chunk-06.log`），校准基线后按轮引导模型分块读取，最多补读 5 轮直至跨过 `483,616` 阈值。达到补读上限仍未跨阈值则记为“场景未触发”。
3. **两类阈值压缩触发路径区分**：区分 `agent_session.rs:5789` 的“轮末真实 `usage` 跨阈值压缩”（本轮无后续工具调用，凭真实响应 `usage.totalTokens > 483,616` 触发）与 `agent_session.rs:3890` `handle_prepare_next_turn` 的“下一内层轮次前投影估算跨阈值压缩”（工具返回大体积 `toolResult` 使投影估算先跨过 `483,616`，在下一模型请求发出前触发），按代理捕获的请求时序分别核验。
4. **分类请求预算与熔断上限**：按 `requestKind` 独立统计上游调用数，不混计用户轮次与内部总结请求。普通对话与工具轮次（`chat_turn`）单用例上限 `8` 次（`D1-1` 上限 `16` 次），历史压缩总结（`compaction_history`）与拆分轮前缀总结（`compaction_turn_prefix`）每次各 `1` 次，分支总结（`branch_summary`）与压缩后续答核验（`post_compact_verify`）各 `1` 次。整套测试设总真实请求熔断上限 `80` 次，超限立即中止并输出代理日志。

### 2.3 单窗口 `bsk` 规范、状态分类与契约分层

1. **单窗口复用与暂停点协议**：全流程仅调用一次 `bsk session start --json` 创建单个后台受控窗口，全部浏览器用例顺序复用同一 `session_id`。涉及运行中并发交互（入队与转插话，或停止、断流与切通道）均采用“到达代理门控 -> 接口确认状态就绪 -> 执行浏览器或 API 操作 -> 释放门控”的四步确定性同步。收到 `user_aborted` 或异常中断时立即进入统一清理流程（`bsk session stop` 与宿主退出）。
2. **直接 API 请求规范**：脚本直接发起的受控辅助请求统一携带同源 `Host` 与 `Origin` 请求头，并附带 `X-CST-Web-Request: 1` 与有效会话 Cookie。写操作每次生成独立 `Idempotency-Key`（测幂等重放时显式复用原键，测业务冲突 `409` 时显式换用新键）。`/api/app/*` 请求携带 `.state/server.json` 中的 `Bearer <appToken>` 与 `X-CST-Client: app`。
3. **四态结果与会话解耦**：每项用例基于独立新建会话或独立预置夹具运行，前序用例失败不污染后续套件。执行结果划分为 `通过`、`失败`（产品行为违背契约）以及 `场景未触发`（模型未走出目标结构）和 `环境阻塞` 四态，失败时保留对应会话 JSONL、代理请求记录与页面快照。
4. **测试层级边界与已知契约覆盖**：本方案聚焦浏览器端到端状态转移、错误提示承接与失败后继续可用性。底层字节级精确临界值与存储层写入故障留在 Rust 契约与单测层（运行命令 `cargo test -p cst-server`），当前覆盖状态如下：
   - **已由契约测试覆盖**：`api_contract.rs` 覆盖 `8 MiB` 慢连接断开（`slow_tcp_sse_client_exceeding_8mib_budget_is_forcibly_disconnected_via_conn_cancel`）、`256` 帧重放与溢出重置（`multiplex_sse_vector_cursor_interleaved_replay_and_subscription_recovery`）及 BitLocker 校验与并发消费（`bitlocker_wakeup_settlement_validates_before_consuming_and_redacts_passwords`）。`exports_contract.rs` 覆盖 `100,000px` 高度拦截（`snapshot_options_date_filtering_and_pagination_contract`）与 HTML 资源限额（`offline_html_parallel_assets_cache_cancellation_and_ssrf_guard`）。`inbox_and_recovery.rs` 与 `session_pool_and_lru.rs` 分别覆盖 `delivering` 崩溃恢复（`crashed_delivering_inbox_item_is_marked_uncertain_on_restart_and_never_replayed`）及 10 会话空闲池淘汰（`unflushed_empty_session_preserves_metadata_across_10_session_lru_eviction_and_restart`）。
   - **契约层待补项（本文不伪称已覆盖）**：`256` 帧、`8 MiB` 与 `32 MiB` 的 `N-1 / N / N+1` 三点精确等值边界单测。另有 `SessionInbox`、`MutationReceipts` 与 `CompactionEntry` 在磁盘满（`ENOSPC`）或只读介质下的落盘失败故障注入单测。

## 3. 测试套件清单

### 套件 D1：真实多轮工具调用、参数修正与过程分段（主宿主 + `bsk`）

本套件验证真实 `deepseek-v4.1-flash` 驱动的多轮工具链与参数强转自修正，并检查 `Length` 截断配对处理和流式定稿连续性。

| 编号 | 状态机路径与边界 | 操作序列与同步控制 | 通过断言与后端核验 |
|---|---|---|---|
| D1-1 | 多轮连续工具链与同轮并发调用 | 在独立会话发送跨文件核查提示词，引导 `deepseek-v4.1-flash` 执行多轮真实工具调用（目标 `>= 6` 轮且含同轮多工具并发，上限 `16` 次模型请求，未走出多轮结构可重跑 1 次）。定稿后刷新页面并点击折叠头 | 内层循环按序回填全部 `toolResult` 并输出结论。折叠头显示真实工具总次数，刷新后首次展开前 `.turn-process-body` 数量为 `0`，点击后正常挂载详情节点 |
| D1-2 | 参数类型三阶段强转与校验错误修正 | 场景 A 由代理按 `(sessionId, chat_turn, 1)` 将真实 `read` 调用的数字参数改写为字符串 `"5"`。场景 B 注入空标题与空数组的 `command_plan` 参数触发校验拦截，放行下一轮真实 `deepseek-v4.1-flash` 读取报错并自主重发 | 场景 A 经 Schema 强转成功执行 `isError: false`。场景 B 不执行工具并落盘带点号路径的 `isError: true` 结果，真实模型在下一内层轮次补齐合法字段成功创建方案 |
| D1-3 | 普通工具与常驻特殊组件交替分段 | 引导真实模型在同一用户回合内依次调用普通检索工具、`sys(overview)`、二次日志检索、`command_plan` 与 `runbook_deliver` 并输出总结正文 | `turnSegments` 切分为 `process` 与 `visible` 交替序列。首段与次段折叠头各自独立统计普通工具次数，`整机概览` 与方案交付卡片前后均不出现“0 次工具调用” |
| D1-4 | `StopReason::Length` 截断配对与恢复边界 | 场景 A 由代理截断末尾 `toolCall` 参数并以 `finish_reason: "length"`（满额 `completion_tokens = maxTokens`）结束该流，放行下一内层轮次透传真实模型。场景 B 注入 `completion_tokens < maxTokens` 的提前 `length` 截断 | 场景 A 残缺工具未实际执行，自动写入配对的 `isError: true` 截断提示 `toolResult` 并继续下一内层轮次由真实模型补发完整调用收敛。场景 B 最多执行 1 次 `is_recoverable_length` 自动压缩重发，不无限重试 |
| D1-5 | 流式增量推进与防抖快照无闪交接 | 向真实模型请求含思考推理、工具调用与多段代码块的回复，在浏览器端逐帧采样聊天区文本长度与正文容器可见性直至定稿后 `500ms` | 流式阶段已渲染正文单调增长不倒退。`message_end` 承接与 HTTP 快照替换期间正文容器持续非空无闪空，最终页面文本与后端 JSONL 完全一致 |

### 套件 D2：500K 超长上下文、自动压缩与溢出恢复（主宿主 + `bsk`）

本套件验证校准后的近 500K 真实上下文推进与两类阈值压缩，同时检查 `Split-Turn` 双总结、溢出恢复及导出限额。

| 编号 | 状态机路径与边界 | 操作序列与同步控制 | 通过断言与后端核验 |
|---|---|---|---|
| D2-1 | 近 500K 真实上下文跨阈值自动压缩与额度面板 | 加载预置多轮历史会话并发出真实探针校准基线至 `[445K, 465K]`。路径 A 在无工具末轮跨过 `483,616` 触发轮末真实 `usage` 压缩。路径 B 通过分块读取 `48 KiB` 日志触发轮前投影估算压缩。点开上下文面板核查后继续发送续答提问 | 两条路径均调用真实 `deepseek-v4.1-flash` 生成摘要并追加 `CompactionEntry`。刚压缩完面板显示“用量未知”（`tokens: null`），续答后回落至压缩后基线且代理捕获的续答 `messages` 包含压缩摘要与保留近期消息。`opencode-go` 三窗口额度展示独立判定记录 |
| D2-2 | 超长单轮工具链 `Split-Turn` 双总结切点保护 | 预置“非空早期多轮对话（保证 `messages_to_summarize` 非空）+ 单轮多次 `48 KiB` 日志读取结果（跨过 `20,000` 保留预算使切点落入单轮内部）”，调用 `/compact` 触发压缩并核查代理请求记录与 JSONL | `firstKeptEntryId` 严格落在 `assistant`（或 `user`）边界，绝不切在 `toolResult` 上。代理精确捕获历史前缀总结与拆分轮前缀总结 2 次真实模型调用，JSONL 摘要含 `**Turn Context (split turn):**` |
| D2-3 | 超过 500K 溢出单次恢复与静默超限处理 | 基于可压缩长会话夹具，场景 A 由代理对首次 `chat_turn` 注入 `context_length_exceeded` 错误，放行真实压缩与重发请求。另在独立夹具连续两次注入该溢出错误。场景 B 注入 `Stop` 且 `prompt_tokens = 505,000` | 场景 A 首次溢出写入 `ContextEditEntry` 裁剪失败消息，经真实模型压缩后自动重发成功。连续二次溢出在 1 次恢复尝试后停止并展示错误提示，会话后续仍可正常发言。场景 B 仅执行后台真实压缩，不重发原用户提问 |
| D2-4 | 手动 `/compact` 冲突边界与双视图核验 | 在新建短会话输入 `/compact` 提交。在刚完成压缩且无新增对话的会话再次执行 `/compact`。随后对照检查聊天页消息流与 `/s/:id/tree`“全部”过滤视图 | 短会话返回 `409 nothing_to_compact`（提示“暂无可压缩内容”），重复压缩返回 `409 already_compacted`（提示“会话已压缩”）。聊天页不渲染压缩卡片且保留早期消息滚动查看，分支树“全部”视图显示 `压缩 · Xk tokens` |
| D2-5 | 130 条消息滑窗分页与离底悬浮锚点 | 打开预置 130 条非 `toolResult` 消息的独立会话，核对首屏挂载轮次数，滚离底部超过 `100px` 观察悬浮按钮，点击顶部“显示更早的消息”后再点击“回到底部” | 首屏仅挂载末尾 80 条消息，顶部按钮显示“显示更早的消息（剩余 50 条）”，点击后增载全部历史并关闭自动跟随。离底超过 `100px` 显示“回到底部”，点击后滚回底部并恢复跟随 |
| D2-6 | 超长会话导出多页切分与 `413` 限额后恢复 | 在 `/s/:id/export` 对正常长会话执行多页 PNG 与离线 HTML 导出，再对高度超 `100,000px` 与快照超 `32 MiB` 的会话发起导出，被拦截后切回正常会话再次导出 | 正常长会话切分生成多张宽度 `2480px` 的连续 PNG 与离线自包含 HTML。超限会话分别返回 `export_too_tall` 与 `413 export_too_large` 并展示明确提示，拦截后导出队列与浏览器进程继续正常服务后续请求 |

### 套件 D3：收发件箱并发、插话边界、响应丢失对账与停止恢复（主宿主 / 辅助宿主 + `bsk`）

本套件验证流式门控期间的队列版本控制与工具批次后的 `steer` 优先投递，并检查外层续答、`abort` 取消和 `202` 丢失对账。

| 编号 | 状态机路径与边界 | 操作序列与同步控制 | 通过断言与后端核验 |
|---|---|---|---|
| D3-1 | 运行中入队与 `version` 乐观锁冲突 | 代理门控挂起首轮真实模型流，确认会话进入 `running` 后连续提交 3 条排队消息，用过期 `version` 调重排接口，再通过页面队列面板编辑正文并拖拽重排，最后释放门控 | 过期 `version` 返回 `409 conflict`。面板编辑与重排携带最新 `version` 更新 `web-inbox`，SSE 实时同步队列顺序，门控释放后按新顺序消费 |
| D3-2 | 串行工具批次完成后的 `steer` 优先投递 | 构造模型单轮返回 2 个工具调用且队列中已有普通 `queue` 待发项 A，在工具批次执行期间将待发项 B 点击“立即”转为 `steer`（`POST /queue/:id/steer`），核查工具执行数与下一模型请求 | 当批 2 个工具完整执行完毕并完成 `finish_turn` 后，`AgentLoop` 在下一内层模型调用边界优先提取 `steer` 项 B 注入真实模型上下文，无需等待整轮结束且优先于普通排队项 A |
| D3-3 | 内层结束后的 `follow-up` 外层续答循环 | 在真实模型生成最终总结流（无后续工具调用）期间门控暂停，投入 2 条普通排队消息（`delivery: "queue"`）确认入队后释放门控 | 当前内层循环正常结束后，外层循环按序提取 2 条 `queue` 消息交由 `deepseek-v4.1-flash` 连续完成两轮独立续答 |
| D3-4 | 流式、退避与压缩阶段的 `POST /abort` 取消恢复 | 分别在模型流式输出期（队列含 2 条 `pending` 项）以及 `429` 退避等待期点击“停止生成”（`POST /abort`），核验上游请求与队列状态，随后发送新消息恢复会话 | 停止后立即取消上游连接或退避定时器，不再发出后续重试请求。收件箱置为 `paused = true` 并展示“排队已暂停 · 发送新消息后继续”，用户主动发新消息后解除暂停并恢复正常对话 |
| D3-5 | `202` 响应丢失对账与附图 `prompting` 窗口锁 | 场景 A（主宿主）经本地网关提交带 PNG 图片的提问，后端落盘接受后由网关故意掐断 `202` 响应触发待确认态，刷新页面并点击重试。场景 B（辅助宿主）在受控慢速 `ImageStore` 读盘窗口期并发提交 `steer` 消息 | 场景 A 刷新与重试按原 `clientMessageId` 查询回执承接状态，真实 `deepseek-v4.1-flash` 仅调用 1 次且正常识别图片内容，迟到错误不降级已确认消息。场景 B `prompting` 锁阻止 `steer` 抢先开启空轮次 |

### 套件 D4：挂起式交互桥接、退避鉴权与真实强杀对账（辅助宿主 / 主宿主 + `bsk`）

本套件验证 BitLocker 合成密码零泄露与通用 UI 四类问答，同时检查瞬态退避、真实 `401` 恢复及双轨强杀重启对账。

| 编号 | 状态机路径与边界 | 操作序列与同步控制 | 通过断言与后端核验 |
|---|---|---|---|
| D4-1 | BitLocker 校验保持挂起、并发消费与全链路隐私（辅助宿主） | 在 `kit-aux` 触发 `bitlocker(unlock)` 密码输入卡，先提交非法格式密码，再用专用合成恢复密码标记并发发起 2 次提交，另测超时请求。完成后扫描全套落盘与传输记录 | 非法输入返回 `400 invalid_bitlocker_input` 且请求保持 `Pending`。并发合法提交仅 1 个返回 `200` 唤醒执行，另 1 个与超时请求返回 `409 expired_request`。扫描 JSONL、`web-inbox` 与代理记录，以及 SSE 抓包、浏览器存储和导出产物，均不含合成密码原文 |
| D4-2 | 通用 `ui_requests` 四类挂起、取消与新幂等键 `409`（辅助宿主） | 通过辅助宿主控制端点依次触发 `select`、`confirm` 与 `input`、`editor` 四类 `ui_requests`，在 `bsk` 页面 `.question-panel` 逐一作答提交。再用**新 `Idempotency-Key`** 重复提交同一已消费 `requestId`，另测问答挂起期间点击停止 | 挂起时底部实时渲染对应提问控件，首次提交返回 `200` 唤醒等待者并收起面板。换新幂等键重放同一 `requestId` 真实命中 `ServerUiBridge` 并返回 `409 conflict`。挂起期点击停止立即取消等待者 |
| D4-3 | `429 / 503` 自动退避重试与双视图隔离（主宿主） | 代理按 `(sessionId, chat_turn, 1..2)` 依次返回 `429` 与 `503`（带 `x-should-retry: false`），第 3 次放行至真实 `deepseek-v4.1-flash`，比对代理记录的第 3 次 `messages` 与前端分支数据 | `AgentSession` 写入 `ContextEditEntry`，第 3 次发往真实模型的 `messages` 已完整剔除前两次失败 `AssistantMessage`，前端分支快照仍保留完整历史事件记录 |
| D4-4 | 真实上游 `401` 鉴权失效广播与凭据恢复（主宿主） | 在 `/settings/provider` 将 `opencode-go` 改为无效测试密钥，回聊天页发送消息触发真实 OpenCode Go 网关 `401`，观察再次发送时的拦截行为，随后填回有效订阅密钥重发 | 真实 `401` 后后端将 `opencode-go` 记入 `invalid_auth` 并广播 `auth_changed`，未修复前新消息直接拦截。填回有效密钥后清除失效标记，真实模型回复恢复正常 |
| D4-5 | 静态遗留夹具与真实进程强杀双轨对账（主宿主） | 轨道 A：预置 `delivering` 收件箱、`rendering` 导出与未闭合 `bitlocker` 夹具启动服务核查。轨道 B：启动真实模型请求并在代理门控挂起（确认 `web-inbox` 处于 `delivering`）时执行 `taskkill /F` 强杀 `cst-server.exe`，原目录重启后用 `bsk` 打开该会话 | 两轨下遗留 `delivering` 条目均转为 `uncertain` 展示“上次投递结果待核对”，重启过程与页面刷新均不向代理重发模型请求。遗留 `rendering` 导出转为 `failed`（`export_interrupted`），未闭合 `bitlocker` 追加 `status: "unknown"` 实时复查记录且不重复执行 |

### 套件 D5：分支树游走、总结中断、派生请求核验与空闲池淘汰（主宿主 / 辅助宿主 + `bsk`）

本套件验证跨压缩点切分支与真实总结中断，并检查 `/fork` 下游上下文隔离和 `SessionPool` 空闲槽位淘汰。

| 编号 | 状态机路径与边界 | 操作序列与同步控制 | 通过断言与后端核验 |
|---|---|---|---|
| D5-1 | 跨压缩点分叉切换、真实总结与请求隔离（主宿主） | 在含压缩节点的会话进入 `/s/:id/tree`，切至压缩点前的早期节点发送新提问，再切回原分支依次验证“总结”与“用自定义提示词总结”并发送下一轮真实提问 | 早期节点新分支脱离原压缩节点独立计算上下文。总结切换调用真实 `deepseek-v4.1-flash` 生成 `BranchSummaryEntry` 并渲染“分支已总结”卡片，代理捕获的下一次真实模型请求包含分支摘要且不含旁支非祖先消息 |
| D5-2 | 分支总结流式生成中途 `abort` 回滚（主宿主） | 在 `/s/:id/tree` 选择另一分支点击“总结”，代理门控挂起 `branch_summary` 流确认页面出现“正在总结所选分支…”后点击“取消分支总结”按钮 | `POST /tree/abort` 中止上游总结流并返回 `cancelled: true`，会话 `leaf_id` 保持原叶节点不变，JSONL 无残缺总结条目，随后仍可重新发起切换 |
| D5-3 | 深层分支 `/fork` 派生与下游模型上下文核验（主宿主） | 在含工具调用与分支总结的会话点击“从此处分支新会话”，选择第 2 轮用户消息确认派生，提交回填草稿触发真实模型回复并检查代理捕获的 `messages` | 新会话仅复制第 2 轮之前的祖先路径并将第 2 轮原文回填输入框。新会话发往 `deepseek-v4.1-flash` 的请求仅含第 1 轮配对完整的历史与本次提问，原会话叶节点不变 |
| D5-4 | `max_idle_sessions = 10` 淘汰与 5 类驻留保护（辅助宿主） | 在 `kit-aux` 建立 5 个受保护活跃会话（流式生成与队列 `pending`，外加活跃 SSE 订阅、BitLocker 挂起与 UI 提问挂起），随后在不建立 SSE 订阅的前提下连续加载 12 个空闲历史会话并释放瞬时租约，最后解除 1 个活跃会话的 SSE 订阅再加载新空闲会话 | 5 个 `is_pinned` 活跃会话全程保留在内存池。12 个无订阅空闲会话按 `last_touched` 顺序淘汰最早的 2 个，保留 10 个空闲槽位。被淘汰会话重开后完整恢复历史，解除 SSE 保护后的会话正常纳入后续 LRU 淘汰 |

### 套件 D6：聚合 SSE 向量游标重放、环形缓冲溢出、慢连接背压与通道切换（主宿主 + `bsk`）

本套件验证真实向量游标断线补齐与 256 帧溢出定向 `reset`，同时检查 `8 MiB` 慢订阅回收隔离和运行中 `AppActive` 切换。

| 编号 | 状态机路径与边界 | 操作序列与同步控制 | 通过断言与后端核验 |
|---|---|---|---|
| D6-1 | 向量游标 `Last-Event-ID` 窗口内无损重放 | 建立聚合流订阅并触发真实模型流式回复，保存已收到的真实帧游标 `global:<e0>:<s0>,<sid>:<e1>:<s1>` 后主动断开连接，待流继续推进约 30 帧（小于 256 帧）后原样携带该 `Last-Event-ID` 重连 | 后端从 256 帧环形缓冲精确补发 `s1` 之后的全部缺失事件，不发送 `event: reset`，数据连续无漏帧 |
| D6-2 | 超过 256 帧环形缓冲触发单会话定向 `event: reset` | 同一 `clientId` 同时订阅会话 A 与会话 B，记录向量游标后断开连接，令会话 A 产生超过 260 帧事件、会话 B 产生 10 帧事件，携带原向量游标重连并观察浏览器端表现 | 后端仅对溢出的会话 A 下发 `event: reset` 触发单会话 HTTP 快照刷新，未溢出的会话 B 直接从环形缓冲补发 10 帧增量，互不干扰 |
| D6-3 | 服务端 `8 MiB` 慢连接背压回收与页面恢复 | 同时保持 `bsk` 页面正常打开与 1 个挂起读取的独立慢速 SSE 订阅客户端，推送高频大体积事件流使慢客户端服务端队列 `queued_bytes` 超过 `8 MiB`。另中断一次页面聚合流验证恢复 | 慢客户端超过 `8 MiB` 时被服务端立即断开回收，同一时刻 `bsk` 页面正常收发与 `/api/health` 零阻塞。页面连接中断重连后按游标窗口自动走增量补发或 `reset` 快照对齐至最新完整状态 |
| D6-4 | 真实工具流运行中切换 `AppActive` 与 `WebActive` | 在输入框填入草稿并启动真实模型多轮工具任务，到达代理门控后用 `App Token` 调用 `POST /api/app/web/close` 切入 `AppActive`，释放门控待后台真实工具与回复落盘后调用 `POST /api/app/web/open` 兑换票据切回 `WebActive` | 切入 `AppActive` 后页面立即断开 SSE 展示“软件端正在使用中”待机页并保留本地草稿。切回 `WebActive` 后自动恢复订阅，完整展示后台执行完毕的工具卡片与真实模型结论 |

## 4. 实机执行结果

2026-11-11 在 Windows 11（i5-12600KF / 32 GB 内存，`bsk 0.4.2`，Chromium `157.0.0.0` 实例 `f627ad94` 视口 `1440×960`）执行 D1 至 D6 共 29 项深度脚本。主宿主 `target\debug\cst-server.exe --standalone-web` 与辅助宿主 `RunningServer` 全程未改动产品源码，真实推理接入 `opencode-go/deepseek-v4.1-flash`（本地 `models.json` 将 `contextWindow` 覆写为 `500000` 以测试 `483,616` 压缩阈值，与上游原生窗口上限区分）。测试结束后已关闭受控浏览器窗口并清理临时凭据。

**29 项现有脚本断言通过，实测发现的 1 项浏览器消息投影契约缺陷已完成修复并补充回归测试（4.2 节），部分原设计扩展断言归入待补证范围（4.3 节）**。

### 4.1 套件与用例核验汇总

| 编号 | 脚本结果 | 实测指标、实际检验边界与证据摘要 |
|---|---|---|
| D1-1 | 通过（附缺陷 #1） | 真实模型连续执行 6 轮共 7 次 `read` 调用（第 2 轮同轮并发 2 次读取，前缀缓存命中 `6,912` tokens）。刷新后用户轮折叠段展开前 `.turn-process-body` 为 `0`，点击 `已工作 38 秒` 后挂载 9 个过程子节点。同时复现首条 `system` 消息产生的空白折叠头（缺陷 #1） |
| D1-2 | 通过 | 场景 A 字符串 `"1"` 与 `"5"` 强转为整型成功读取（`isError: false`）。场景 B 空标题与未知字段触发校验拦截且未执行工具，下一内层轮次真实 `deepseek-v4.1-flash` 读取报错后自主补齐字段成功创建方案（`call_7de06c03207c42c5ae4a676d`） |
| D1-3 | 通过 | 单回合真实调用 `read -> sys(overview) -> read -> command_plan -> runbook_deliver`，DOM 切分为 3 段过程头（`已工作 46 秒`、`已完成 1 次工具调用`、`思考过程`）与 3 段常驻卡片交替挂载，全程无 `0 次工具调用` |
| D1-4 | 通过 | 场景 A 满额 `length` 截断未执行残缺 `read`，写入配对 `isError: true` 截断提示并在下一内层轮次由真实模型补发完整调用收敛。场景 B 提前 `length` 截断触发 1 次真实自动压缩（`compaction_history = 1`）并重试 1 次后按上限停止（`chat_turn = 2`） |
| D1-5 | 通过 | `MutationObserver` 加 `25ms` 定时采样共 51 次，流式阶段可见字符单调增至 `1,007` 且含 2 段代码块，采样期内未见闪空，定稿包含 `COMPLETE_D1_5` 标记（JSONL 原文 `1,074` 字符含 Markdown 语法符号，未做逐帧 `rAF` 与归一化全文逐字比对） |
| D2-1 | 通过 | 实测分词密度为 `11,822 tokens/轮`。路径 A 真实探针基线 `455,228`，经无工具轮次递进（`463,880 -> 472,363 -> 480,848 -> 489,971`）凭真实上游 `usage` 跨过 `483,616` 触发轮末压缩，压缩后瞬间面板显示 `用量未知`（平均缓存命中率 `50.5%`），续答回落至 `47,580` 且请求含压缩摘要。路径 B 真实探针基线 `466,998`（略高于原计划 `445K–465K` 区间），工具轮上游实测用量为 `467,218`，并发读取 3 份 `48 KiB` 分块使本地投影估算达 `503,086`（非上游实测 `usage`），在下一 `chat_turn` 前触发 `compaction_history`（`462,036`）与 `compaction_turn_prefix`（`476`），随后模型请求降至 `79,677` |
| D2-2 | 通过 | `/compact` 切点 `firstKeptEntryId` 落在 `a-giant-step-3`（`role: "assistant"`），未切在 `toolResult`。代理捕获 `compaction_history` 与 `compaction_turn_prefix` 两次真实调用，摘要包含 `Turn Context (split turn)` |
| D2-3 | 通过 | 场景 A1 首次 `400 context_length_exceeded` 写入 `ContextEditEntry` 并经真实压缩重发恢复。场景 A2 连续二次溢出在 1 次压缩与 2 次尝试后停止并渲染错误横幅，后续新提问正常回复。场景 B `505,000` 静默超限仅后台压缩 1 次，未重发原提问 |
| D2-4 | 通过 | 短会话与刚压缩完会话分别返回 `409 nothing_to_compact` 与 `409 already_compacted`。聊天流隐藏压缩卡片并保留早期历史，`/tree` 全部视图显示 `压缩 · 48k tokens` |
| D2-5 | 通过 | 130 条历史首屏挂载 40 轮（80 条），顶部显示 `显示更早的消息（剩余 50 条）`，点击后完整增载 65 轮，点击 `回到底部` 滚回底端（`dist = 0px`） |
| D2-6 | 通过 | 正常长会话切分导出 5 张多页 PNG 与自包含离线 HTML。超 `100,000px` 会话报 `export_too_tall`，超 `32 MiB` 会话返回 `413 export_too_large`，拦截后再次发起正常导出顺利完成 |
| D3-1 | 通过 | 门控运行期入队 3 项，旧 `version` 重排返回 `409 conflict`。通过面板编辑首项并拖拽将第 3 项移至队首（`version: 5 -> 7`），释放门控后真实模型按 `Q3 -> Q1_EDITED -> Q2` 顺序消费 |
| D3-2 | 通过 | 当批 4 个工具（含 `sys` 实采与 `read`）全部执行完毕并落盘 4 条 `tool` 结果后，`steer` 项在下一内层模型调用边界注入上下文，普通 `queue` 项留待外层续答循环 |
| D3-3 | 通过 | 主轮结束后外层循环自动按序提取 2 条普通排队消息，驱动真实模型完成共 3 轮连续独立回复 |
| D3-4 | 通过 | 流式期点击停止立即中断上游流并保留排队项且置 `paused = true`，发新消息后恢复消费。`429` 退避窗口期点击停止在 `647ms` 内取消 `retry_cancel` 定时器，等待过原 `2,000ms` 窗口确认第 2 次请求未发出（本轮未单独测压缩生成中途 `abort`） |
| D3-5 | 通过 | 场景 A 通过 `window.fetch` 在服务端真实返回 `202` 后抛出 `TypeError` 模拟应用层响应丢失，经 API 原幂等键重放（`202`）与页面刷新完成对账，真实请求仅发出 1 次且含 `image_url`（未点气泡重试按钮，未断言颜色词文本）。场景 B 辅助宿主慢速附图窗口内 `busy = false` 且 `prompting = true`，并发 `steer` 未抢跑 |
| D4-1 | 通过 | 辅助宿主经 HTTP 接口提交错误动作与非法密码均返回 `400 invalid_bitlocker_input` 且保持挂起，并发提交合成密码返回 `[200, 409]` 单次唤醒解锁，超时返回 `409 expired_request`。已扫描的落盘目录、SSE 抓包、浏览器存储与代理 `320` 字符预览未检出密码标记（未测页面密码框真实键入清空及完整请求体扫描） |
| D4-2 | 通过 | `.question-panel` 依次完成 `confirm`、`select`、`input` 与 `editor` 四类问答唤醒，挂起取消立即收起面板并返回 `null`。换用新 `Idempotency-Key` 重放已消费 `requestId` 真实命中桥接器并返回 `409 conflict` |
| D4-3 | 通过 | 注入 `429` 与 `503` 后第 3 次放行真实模型返回 `200`，JSONL 写入 2 条 `ContextEditEntry`，代理记录第 3 次请求 `roles` 为 `["system", "user"]`（已剥离两次失败助手半轮），`/tree` 保留 16 个历史节点 |
| D4-4 | 通过 | 在 `/settings/provider` 填入无效密钥后触发真实 OpenCode Go 网关 `401`，后端标记 `requiresLogin = true` 并在修复前直接预检拦截新请求（`400`）。填回有效订阅密钥后清除失效标记，真实模型回复恢复正常 |
| D4-5 | 通过 | 轨道 A 静态夹具验证 `delivering -> uncertain`、`rendering -> export_interrupted` 及未闭合 `bitlocker` 追加 `cst.bitlocker-state-verification`（`unknown`）。轨道 B 在 `before_upstream` 门控期确认 `delivering` 落盘后对 `cst-server.exe`（PID `74052`）执行 `taskkill /F`，重启（PID `78752`）并刷新页面均显示 `上次投递结果待核对` 且零重放（属上游副作用发生前的进程强杀对账） |
| D5-1 | 通过 | 切至压缩点前节点新分支脱离原压缩摘要独立请求。主干与旁支间完成普通真实总结与自定义提示词真实总结（`branch_summary = 2`），代理 `320` 字符预览核验下游请求含分支摘要且不含旁支用户消息哨兵 |
| D5-2 | 通过 | 分支总结门控挂起期间点击 `取消分支总结`，上游连接立即中止，`leafId` 保持原节点 `c010f350` 不变，JSONL 零残缺总结条目，后续切换正常可用 |
| D5-3 | 通过 | 通过 `ForkPicker` 选中第 2 轮用户消息派生新会话，横幅与草稿回填正确。提交草稿后代理结构化快照核验新会话保留第 1 轮配对 `toolCall + toolResult`，且 `320` 字符预览不含旧分支第 2 轮与第 3 轮哨兵，原会话 `leafId` 不变 |
| D5-4 | 通过 | 5 类活跃会话全程 `pinned = true` 保留在池内。连续打开释放 12 个无 SSE 订阅空闲会话后精确淘汰最早的 `idle-sess-01` 与 `idle-sess-02`（总槽位数 `15 = 5 + 10`）。解除 `pin-sse` 订阅后正常纳入淘汰，重开被淘汰会话完整恢复历史 |
| D6-1 | 通过 | 真实流式回复中途捕获向量游标 `global:...4b:3,01a1ca6c...:4` 后断线，流结束后携带该 `Last-Event-ID` 重连，收到连续无缺口重放帧 `[5, 194]` 共 190 帧且 `reset = 0`（未额外比对服务端最终内部序号上限） |
| D6-2 | 通过 | 辅助宿主受控注入下，双会话聚合连接断线期间向会话 A 推送 265 帧、向会话 B 推送 10 帧，重连后仅会话 A 收到定向 `event: reset`，会话 B 补发 10 帧增量（属服务端协议级验证，非多标签 `SharedWorker` 验证） |
| D6-3 | 通过 | 辅助宿主受控推送 `30 × 512 KiB` 全局事件期间，挂起不读的慢速 TCP 客户端在第 19 帧被服务端断开回收，单次并发 `/api/health` 耗时 `15ms`，洪峰结束后 `bsk` 页面正常完成真实消息收发 |
| D6-4 | 通过 | 真实 `sys(overview)` 工具流门控期间填入草稿并切入 `AppActive`，页面立即展示 `软件端正在使用中` 待机页。后台完成真实工具采集与模型总结后兑换单次票据切回 `WebActive`，完整展示 `整机概览` 卡片与模型结论，并保留原输入框草稿 `待保留的现场草稿_D6_4_9988` |

### 4.2 实测发现缺陷的修复与回归覆盖

1. **缺陷 #1（已修复：会话快照暴露 `role: "system"` 导致首屏渲染空白 `已完成` 折叠块）**：
   - **成因定位**：`crates/pi-session/src/agent_session.rs:5123` 在首次真实模型请求及后续提示词变更时落盘 `AgentMessage::System`（`role: "system"`，`content: ""`，承载 `sections` 补丁与工具装载元数据）。原 `crates/cst-server/src/session/slot.rs` 构造会话快照与广播 SSE 时未过滤该内部角色，前端 `conversation.tsx` 与 `conversation-parts.ts` 遂将其归入助手轮次渲染空白折叠头。
   - **修复实现与回归测试**：JSONL 持久化、模型投影与 `/tree` 原始拓扑保持不变。后端 `slot.rs`（及 TS 对照实现 `agent/home/extensions/web/server/api.ts`）通过 `is_browser_visible_message` 仅向快照 `messages / entries` 与浏览器 SSE 输出 `User / Assistant / ToolResult`（`BranchSummary` 独立投影）。前端 `conversation-parts.ts` 新增 `isConversationMessage` 显式角色白名单并在 `groupTurns` 仅将 `assistant` 归入孤立助手轮次，`conversation.tsx` 与 `export-document.tsx` 同步复用。已由 `crates/cst-server/tests/api_contract.rs`（`session_snapshots_exports_and_fork_omit_system_messages_while_preserving_jsonl_and_tree`）与 `src/web/frontend/app/conversation/conversation-parts.test.ts` 回归锁定，并重编更新 `cst-server` 与 `agent/home/extensions/web/static`。
2. **观察项 #1（`opencode-go` 额度面板状态）**：`D2-1` 打开上下文面板时，`opencode-go` 命中 `quotaProviders` 显示额度分区，当前环境返回 `暂无额度数据`，上下文容量与平均缓存命中率（`50.5%`）显示正常。

### 4.3 待补证断言与独立验收边界

| 类别 | 具体待补项 |
|---|---|
| 代理账本与预算统计 | 本轮按套件分进程运行导致 `80` 次熔断计数各自重置，`messagesSnapshot` 仅内存保留前 `320` 字符预览。后续需改为跨进程共享持久化脱敏账本（记录 `runId`、完整请求体哨兵扫描结果与实际转发计数） |
| 现有用例细化补证 | `D1-5` 归一化全文逐字比对与 `rAF` 逐帧采样、`D3-4` 压缩阶段 `abort`、`D3-5A` 点击气泡重试按钮与图片颜色词核验、`D4-1` 页面隐藏密码框填入后清空与导出 `completed` 强断言、`D6-1` 补发末帧与服务端最终序号等值断言、`D6-3` 销毁套接字前先断言服务端主动断开 |
| 独立发布验收项 | 多标签页共享 `SharedWorker` 端到端交互、真实管理员 BitLocker 加密卷解锁与解密、Windows 10 / Chromium 111 兼容性、窄视口与输入法组合输入、正式发行包形态验收 |



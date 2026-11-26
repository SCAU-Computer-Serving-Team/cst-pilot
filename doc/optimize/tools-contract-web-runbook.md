# 工具契约统一、联网检索与 `runbook` 重设计

本文记录阶段 9.3 中全工具运行时间与错误契约收敛、`read` 工具 JSON 化、联网检索工具重设计及单工具 `runbook` 改造的背景、决策与对照验收项。架构决策见 [ADR-019](../ADR.md#adr-019全量工具注入-elapsedmsread-改-json-并统一错误与降级)、[ADR-020](../ADR.md#adr-020联网检索重设计为-exa-与-deepseek-双提供商三工具体系) 与 [ADR-021](../ADR.md#adr-021收敛为单工具-runbook双端卡片支持一键导出文本)，总览进度见 [9.3.optimization.md](../rust/plan/9.3.optimization.md)。

## 一、背景

全仓工具集在运行时间可观测性、错误与降级口径、联网检索能力和修复方案交付四方面存在结构性缺口：

1. **工具耗时盲区与 `read` 格式分裂**：17 个白名单工具中仅 `disk(usage)` 的 `native-walk` 输出 `stats.elapsedSec`，`$MFT` 路径与其余 16 个工具未向模型返回耗时。Web 端历史会话重放时卡片丢失耗时。`read` 返回纯文本且含上游无效的 `bash sed` 提示，与其余 JSON 工具契约不统一。
2. **错误与降级口径冲突**：`sys(gpu)`、`sys(sensor)` 正常时输出空 `counterErrors: {}`，`bitlocker(status)` 输出空 `collectionErrors: []`，双端卡片将空容器误判为 `degraded`。`ls` 的 `$MFT` 回退吞错、`ffgrep` 达 `256 MiB` 上限丢弃命中、`tools/mod.rs::json_result` 超 `50 KiB` 直接报错，均未遵循局部降级原则。
3. **联网工具遗留冗余与能力不足**：原实现继承自旧 `pi-web-access`，`source_check` 与 `web_search` 实现重复且与双端 `artifact/claims` 卡片脱节。多词与多链接串行执行，单条超时或 403 即中断整次请求。缺少 Exa 付费深度搜索与 DeepSeek Search API 接入，直连抓网页遇到 GB18030 中文页面报错。
4. **`runbook` 三工具割裂与二次推理开销**：`command_plan`、`runbook_deliver`、`runbook` 同时开放，入参重叠且各自复制提示词。队员核对完卡片命令想导出 `.txt` 时，必须再发消息触发一轮模型调用。落盘同时存在 UUID 子目录与序号目录，`commands[].shell` 枚举大小写分裂。

## 二、决策

白名单工具由 17 个收敛为 14 个（文件存储 5、系统诊断 5、联网检索 3、方案交付 1），统一外层契约并重设计联网检索与 `runbook`。

### 2.1 全工具统一耗时、`read` JSON 化与错误/降级契约

1. **外层统一注入整数毫秒 `elapsedMs`**：
   - 在 `crates/cst-kit/src/kit.rs` 的全工具包装层统一计时，覆盖全部 14 个工具。
   - 正常、降级、错误与 `50 KiB` 裁剪结果均在顶层注入整数 `elapsedMs`。工具报错统一转为结构化 JSON（`{"error": "...", "elapsedMs": N}`）加 `isError: true`，便于区分毫秒级拒绝与超时失败。
   - 收敛移除 `disk(usage).stats.elapsedSec`，`stats` 仅保留 `filesScanned` 并为 `$MFT` 补齐。保留 `sys(proc/io).intervalSec` 反映采样窗口。Web 端卡片历史重放优先读取 `durationMs ?? details.elapsedMs`。
2. **`50 KiB` 受保护键裁剪统一化**：
   - 将 `tools/mod.rs::json_result` 并入 `diagnostics/result.rs`，废除超限硬报错。
   - 新增受保护顶层与交付键白名单（`elapsedMs`、`degraded`、`degradedFrom`、`error`、`notice`、`file`、`dir`、`name`、`runbookId`、`version`、`sequence`、`action`、`level`、`levelLabel`、`items`），超限时优先裁剪大数组与正文，确保交付路径与状态完整。
3. **`read` 工具 JSON 化与图片双块兼容**：
   - 保持上游 `pi-runtime` 源码不动，在 `crates/cst-kit/src/tools/` 新增薄封装。
   - 文本文件返回单块 JSON，含 `path`、`kind: "text"`、`offset`、`limit`、`outputLines`、`totalLines`、`outputBytes`、`truncated`、`truncatedBy`、`nextOffset`、`content`、`notice?`、`elapsedMs`，扫描阶段预留 JSON 转义余量以控制在 `50 KiB` 内。
   - 图片文件返回 `[Text(meta_json), Image(base64)]` 双块，`details` 仅存轻量 `meta_json` 不重复存 base64。已核验 `pi-ai` 全部 4 条模型协议链路原生兼容，双端卡片改按 `detail.kind == "image"` 隐藏原始文本并渲染 `detail.content`。
4. **错误与降级统一规则**：
   - 请求数据源全部失败返回 `isError: true`，至少一路成功返回 `isError: false` 加 `degraded: true`。
   - 正常调用不序列化 `degraded` 与空错误容器（`counterErrors`、`collectionErrors`），双端卡片同步加固非空判断。
   - 补齐 `ls` `$MFT` 回退的 `degradedFrom` 与 `degraded: true`；`ffgrep` 达 `256 MiB` 上限时保留已命中项并置 `truncated: true` 与 `degraded: true`。清理 `app/prompts/APPEND_SYSTEM.md` 失效的 `agent/home/` 旧路径。

### 2.2 联网检索重设计（Exa 默认 + DeepSeek Search API 可选）

1. **三工具收敛**：保留 `web_search`、`web_fetch`（原 `fetch_content` 更名，卡片兼容旧历史会话渲染）、`get_search_content`，删除 `source_check`。
2. **双提供商能力对接**：
   - **Exa（默认提供商）**：未配置付费 Key 时走托管免费 MCP 回退，保证便携 U 盘开箱即用。配置付费 Key 后调用 `https://api.exa.ai` 的 `POST /search`（支持 `type: lite / auto / deep-lite / deep`、`contents.maxAgeHours`、`highlights`/`text`/`summary`、域名与时间过滤；`lite` 模式自动剥离不支持字段防 `400`）与 `POST /contents`。
   - **DeepSeek Search API（可选提供商）**：对接官方独立 JSON REST 端点 `https://api.deepseek.com/v1/tools/search`（查询长度 `1–128` 字符守卫，支持 `site:`/`after:`/`before:`/`lang:` 算子、`count` `1–50`、`ori_question`、`need_content`）与 `POST /v1/tools/fetch`。支持独立 API Key 或开启 `reuseModelApiKey` 复用模型凭据。
3. **并发、容错与抓取策略**：
   - 多词（`1–3`）与多链接（`1–5`）改受控并发并保持输入顺序，单条失败记入该项 `error` 与 `collectionErrors` 并标 `degraded: true`，仅全败返 `isError: true`。
   - 解耦常规超时（默认 `15 s`）与深度搜索超时（默认 `45 s`）。所有 URL 发起前先经本地 SSRF 策略与 DNS 解析预检，拒绝内网域名及私网地址外发至外部接口。直连抓取拒绝二进制媒体类型并支持 UTF-8 与 GB18030 解码。
4. **配置与服务端接口**：
   - 升级 `data/config/web-search.json` 为 v2 结构并兼容读取 v1，单文件上限 `64 KiB`，保留在 `PathGuard` 敏感保护清单内。`baseUrl` 仅允许 HTTPS，换域禁止继承旧 Key，`reuseModelApiKey` 仅限官方 `https://api.deepseek.com`。
   - 服务端新增 `GET/PATCH /api/settings/web-search`（密钥脱敏，显式 `clearApiKey: true` 清空，保存后热生效）与 `POST /api/settings/web-search/test`。双端图形设置页等待设置页重设计 PR 合并后接入。

### 2.3 单工具 `runbook` 与双端一键导出

1. **工具合并与命名统一**：合并 `command_plan`、`runbook_deliver`、`runbook` 为单工具 `runbook`，删除 `sequence_store.rs`。`commands[].shell` 统一输出小写 `"cmd" | "powershell"`。
2. **可读落盘路径与版本保护**：取消 UUID 子目录，按当天分配序号 `01–999`，统一落盘至 `artifacts/<YYYY-MM-DD>/runbook/<序号>-<风险档>-<标题>-v<版本>.txt`。修订旧方案沿用原 `runbookId` 与序号并递增 `version`，保留 `baseVersion` 乐观锁与现场修改防覆盖检查。
3. **双端卡片一键直出 `.txt`**：在 `crates/cst-server/src/runbook.rs` 新增 `POST /api/sessions/:id/runbook/export`。队员核对卡片命令后点击按钮直接生成 `.txt` 并用记事本打开，不消耗额外模型推理。保留模型参数显式导出路径以适配无界面场景，卡片保留旧 `command_plan` 与 `runbook_deliver` 历史记录只读渲染。

### 2.4 明确放弃的方案

- **统一耗时使用浮点秒 `elapsedSec`**：毫秒级本地缓存命中会被舍入为 `0.0`，失去区分缓存与阻塞的能力。
- **保留 `source_check` 别名**：与 `web_search` 实现重复且造成前端卡片分叉。
- **接入 DeepSeek Chat 内置绑定工具**：该形态强绑定 DeepSeek 模型并屏蔽采样参数，不利于多模型切换。
- **在 `read` 的 `details` 中重复存图片 base64**：会导致会话 JSONL 文件体积翻倍。
- **保留 `command_plan` 与 `runbook_deliver` 双工具及 UUID 导出目录**：增加选型噪声、导出轮次与 U 盘寻址成本。

## 三、实测与验收对照

### 3.1 改造前后关键维度对比

| 改造维度 | 改造前 | 改造后设计 | 实测结果（`2026-11-26`） |
|---|---|---|---|
| 白名单工具总数 | 17 个（含重复与割裂工具） | 14 个（职责无重叠） | 已验证 `OPEN_TOOL_NAMES.len() == 14`，契约与双端单测全过 |
| 模型可见工具耗时 | 仅 `disk(usage)` `walk` 有浮点秒 | 全量 14 工具顶层整数 `elapsedMs`（含错误与裁剪） | 已验证正常、降级、错误 JSON、图片双块与裁剪输出均含 `elapsedMs`，Web/桌面历史卡片支持回退显示 |
| 健康调用假降级 | `sys(gpu/sensor)`、`bitlocker` 100% 误报 `degraded` | 省略空错误容器 + 非空判断，假降级 `0%` | Release 实测 `sys(gpu/sensor)` 输出 `counterErrors=null`、`degraded=None`，双端非空判断单测全过 |
| `read` 工具返回与图片 | 纯文本（含无效 `bash sed` 提示） | 全 JSON；图片 `[Text(JSON), Image]` 双块兼容 | `read.rs` 单测通过：无 `bash sed` 提示，图片双块 `details` 不含 base64 |
| 联网多词/多链容错 | 串行执行，单条失败整次报错 | 并发执行，单条失败局部降级保留成功项 | `web_access.rs` 与 `tools_and_security.rs` 验证并发局部降级、顺序保持与全败 `isError` |
| 联网提供商能力 | 仅免费 Exa MCP，无深度搜索，中文 GB18030 报错 | 默认 Exa（免费回退 + 付费 `deep`）+ 可选 DeepSeek API + GB18030 解码 | v1->v2 迁移、Exa `lite` 参数守卫、DeepSeek 128 字符守卫、SSRF 云端预检、GB18030 解码与 `/api/settings/web-search` 脱敏接口全部通过 |
| `runbook` 核对后导出 | 需再发消息触发 1 轮 LLM 调 `runbook_deliver`，落盘带 UUID 目录 | 双端卡片一键调本地接口导出（`< 10 ms`），统一 `01-风险档-标题-v1.txt` | `runbook_and_plan`、`runbook_session`、`api_contract`（`POST /api/sessions/:id/runbook/export`）与 Release 实测 `01-较安全-网卡重置方案-v1.txt` 全部通过 |
| `50 KiB` 裁剪安全 | `tools::json_result` 硬报错；`runbook` 长命令截断路径 | 受保护键白名单优先保留路径、ID、状态与 `elapsedMs` | `protected_keys_survive_50kib_reduction` 单测通过，裁剪后保持合法 JSON 与核心交付字段 |

### 3.2 验证结论

1. **批次 1 验证**：`cst-kit` 59 项单测与 `diagnostics_contract`、`tools_and_security` 通过；Web 前端 `lint`、`typecheck`、`test` 与 `build` 全过；`node --test src/test/*.test.mjs` 51 项通过。
2. **批次 2 验证**：`web-search.json` v1/v2 兼容、`GET/PATCH /api/settings/web-search` 密钥脱敏、Exa 与 DeepSeek 参数守卫、SSRF 与 GB18030 回归测试全部通过。
3. **批次 3 验证**：单工具 `runbook` 序号继承、乐观锁与防篡改、服务端 `/api/sessions/:id/runbook/export` 及双端卡片一键导出验证全部通过。

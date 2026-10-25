# 测试指南

本目录说明测试分工、执行入口、隔离要求和判定规则。测试结果、日期日志和运行产物放仓库外当天临时目录，不在本目录维护通过数量或验收进度。

## 测试分工与入口

以下命令从仓库根目录执行。

| 模块 | 入口 | 职责 |
|---|---|---|
| 工具与工程约束 | `node --test src/test/*.test.mjs` | 工具行为、工程配置、画布结构和文档链接 |
| 发行 | `node --test pack/test/*.test.mjs` | 启动器、环境隔离、白名单、配置与资源完整性 |
| OAuth | `npm run test:oauth --prefix agent` | 登录、续期、模型目录与凭据隔离 |
| Web 后端 | `npm run test:web --prefix agent` | API、会话、存储、鉴权、队列和故障恢复 |
| Web 用户链路 | `npm run test:web:e2e --prefix agent` | 隔离浏览器中的完整操作流程 |
| 跨端共享 | `npm run test:web:shared-audit --prefix agent` | 独立 Pi 进程、共享配置、历史目录与单写权 |
| 前端 | `npm test --prefix src/web/frontend` | 状态、数据转换与交互逻辑 |
| 字体与静态资源 | `npm run test:fonts --prefix src/web/frontend` | 字体覆盖、许可、校验和及资源更新 |
| 遥测 | `npm run test:telemetry --prefix agent` | 采集、草稿、扣点、队列、重试与 TLS |
| 遥测跨仓库 | `npm run test:telemetry:e2e --prefix agent` | 真实 Pi 到两个 Go 接收端，需配置接收端可执行文件 |

格式、类型与静态检查独立执行：`npm run check --prefix agent`、`npm run check:engineering --prefix agent`、前端 `lint` 与 `typecheck`。这些检查不代替功能验收。

## 专项说明

| 文档 | 内容 |
|---|---|
| [启动器](launcher.md) | 管理员授权、WT、参数、管道和发行验证 |
| [账号](account.md) | OAuth／APIKEY、本人资料、额度、模型与真实服务边界 |
| [Web 用户链路](web-e2e.md) | 浏览器测试、故障注入、实机交互与产物 |
| [共享后端](shared-backend.md) | TUI/Web 配置、会话写权与进程边界 |
| [遥测](telemetry.md) | 发送端、OA、接收端、账单与双端集成 |
| [环境覆盖](Testlist.md) | 设备、系统、介质与 PE 的选样要求 |

## BitLocker 验收

自动入口：`node --test src/test/bitlocker.test.mjs`。测试通过真实注册工具和 Pi 会话覆盖 TUI / Web 输入、操作确认、逐卷结果、调用中断、同卷互斥与 JSONL 凭据脱敏。Windows 操作使用替身，PowerShell 程序仅替换 CIM 命令；连接选项和标准输入传参另行核查。

实机先做只读 status，并用独立 PowerShell 对照权限与错误。unlock、decrypt 只在获准的隔离测试卷上验证；实际终端、加密卷、Windows 10/11 和 PE 的验收范围见 [Todo](../Todo.md)，不要把替身通过表述为真实磁盘执行成功。

特殊组件的状态、详情展开、复制、窄屏可用性、固定目标与操作恢复见 `agent/home/extensions/web/test/special-tool-states.e2e.ts`。流式回归见 `streaming-after-tool.e2e.ts`；文件打开、请求去重与凭据提交边界见 `special-tool-api.test.ts`。

## 自动测试取舍

- 保留公开行为、故障恢复、持久化、鉴权与凭据边界；跨端一致性直接对照 Pi。
- 画布只检查颜色登记、节点完整性，文档只检查本地链接。不锁定节点 ID、演示内容、CSS 原文、文档标题或句式。
- 相同行为已有完整链路覆盖时，不再增加重复用例。字号、间距、颜色、动画时长和曲线按设计变更人工验收；自动测试只判断内容可见、可操作、不重叠和不溢出。
- 不用源码字符串、内部文件数量或 DOM 节点身份证明功能正确。安全发行边界、数据格式、资源完整性与明确工程规则仍独立校验。

## 选择范围

- **局部修复**：阅读相关代码，执行小脚本、参数边界或故障注入；记录未做的实机验证。
- **环境验证**：执行 B01–B35 公共组，再执行该环境对应的差异项。不适用的项目写明原因。
- **发布前覆盖**：根据 Testlist 补足目标环境，不把单台样机通过推广到全部设备或系统版本。

本页是方法说明，不要求每次文档或代码修改都执行全量测试。

## 执行与判定

1. 记录工具版本/提交、系统版本、权限、硬件、启动介质及目标路径。
2. Agent 仅通过项目工具采集；核查者通过独立 PowerShell、系统界面或计时取得对照证据。
3. 对照同一对象、相近时间和相同单位。实时采样不能要求逐值相等；冲突先排除权限、口径、采样窗口和对象不同，再记录缺陷。
4. 每项记录调用、关键返回、对照方法、结论和耗时。只读核查，不修改机主数据；需要拔插、准备大文件或调整环境的项目单独安排。
5. 将结果、日志和截图放仓库外当天临时目录；区分通过、预期降级、失败、未执行和不适用。

明确且符合设计的降级可以通过。崩溃、挂起、静默丢失错误、把未知数据当作正常值应判失败。硬件不存在本身不算失败，但必须与采集失败区分。

## 公共测试组 B01–B35

调用栏使用简写，完整参数见 [工具文档](../tool/README.md)。`<Win>` 为系统 Windows 目录，`<Kit>` 为工具包目录，`<Sys>` 为系统盘符，`<Vol>` 为工具包所在卷根。

### disk

1. **B01** space，全卷：与可访问卷对照；free≤total，剩余 0 保留为 0。
2. **B02** info，指定 Kit 所在盘：volumes 仅目标卷；物理盘匹配关联。关联失败时全量物理盘须带明确说明。
3. **B03** health，指定 Sys：对照目标盘；保留可用数据和实际错误。分别验证普通/管理员权限，不预设普通权限必失败或管理员必有数据。
4. **B04** usage，Kit，top=10：WizTree 成功时四类表齐全；Node 降级仅有目录排行，来源和限制明确；大小与同口径统计相符。
5. **B05** usage，Win/WinSxS，top=5，连续两次：记录冷/热耗时和结果量级；usage 仍会扫描，不要求第二次必然更快。
6. **B06** usage，不存在路径：明确 error，不崩溃或挂起。
7. **B07** all：space、info、health 均有结果或具体失败原因，成功数据不被其他失败吞掉。

### ls

1. **B08** Kit/pwsh，top=15：大小排序、截断项数和 omitted 合计正确；未知大小保持 null。
2. **B09** Win/WinSxS，top=10：B05 后验证已扫描路径复用；跨卷不串用缓存，记录耗时。
3. **B10** Vol 卷根：文件和目录均可枚举；不可读项、预算停止和统计下界如实说明。

### sys

1. **B11** overview：可用 CPU 值在 0–100；mem.used≤total；机型字段合理，采集失败有错误。
2. **B12** proc，top=15：byCpu/byMem 各不超过 15；按实际间隔计算，与同口径工作集/CPU 核查。
3. **B13** gpu：adapters 与设备清单对照；gpuPct 不累加超 100；多卡分别返回；NVIDIA 缺失/失败允许 LHM 补充，数据源失败彼此独立。
4. **B14** sensor：结构和单位正确；允许无可读传感器，counterErrors 如实；低频率不直接判过热。
5. **B15** io，连续两次：分别记录冷/热耗时；磁盘与进程指标口径清楚，不要求跨调用值或进程数相等。

### startup

1. **B16** startup，无参数：三类清单齐全或有具体采集错误；disabled 三态，按来源和类别核查同名项，不将 Run 状态套给 RunOnce。

### driver

1. **B17** problem：成功查询允许空数组；失败不能伪装无异常。
2. **B18** core：网络、蓝牙、音频、显示和服务/驱动字段可核对。
3. **B19** external：与 USB、蓝牙、显示和可移动存储枚举对照，不将全部条目认作外接。
4. **B20** find class=Net → 取设备 ID 子串 → find id：原设备命中；另覆盖只出现在 HardwareID 的子串及引号、反斜杠。
5. **B21** find name+class：结果是仅 name 结果的子集，验证 AND；名称按字面子串匹配。
6. **B22** find，无条件：明确提示至少传一个条件。

### eventlog

1. **B23** recent，hours=48：时间倒序；counts 未截断时 sum(n)=total。
2. **B24** boot，kind=bluescreen：ID/提供程序范围正确；有样本时 firstTime/lastTime 对应样本首尾；空样本不要求时间字段。
3. **B25** crash，再按 app 过滤：按提供程序/ID/级别配对；Information 级 WER 不漏报；消息过滤覆盖全部分组，空命中合法。
4. **B26** service：SCM 故障清单正确；7045 新装服务属于 boot，不能误作服务运行失败。
5. **B27** disk：指定 ID 范围与 System 记录一致；空命中合法。
6. **B28** query，ids=[41]，logName=System，top=20：ID 查询、排序、截断与计数一致。
7. **B29** detail，取 B23–B28 的 logName+recordId：id/provider/recordId 与样本一致；记录已轮转则如实未找到。
8. **B30** security：普通权限明确 degraded；管理员查询成功或返回实际错误，不能伪造空结果。
9. **B31** security，type=lockout 或 logonFail：仅对应 ID；有权限时空命中合法，无权限先报告降级。

### runbook

1. **B32** safe 档，两条命令：文件落在 `outbox\<当天>\`；文件名含递增序号、中文档名与标题；打开无 Markdown，文件头一行概括，每条四行：环境、说明、命令标签、命令正文；正文独占一行；UTF-8 BOM + CRLF，记事本无乱码。
2. **B33** 同日再调用两次；随后用已有序号修正其中一份；跨天再调用一次：序号递增为 02、03 且不重复；沿用序号重写后文件名随新标题变、序号不变、文件数不增；跨天从 01 重新开始。
3. **B34** command 含换行、title 为空、items 为空：整次调用失败并说明原因，不产生半成品文件。
4. **B35** 介质只读或 outbox 不可写：返回具体失败原因，不静默；对照 outbox 与机主系统无其他新增文件。

## 记录模板

在仓库外当天临时目录创建记录，注明测试范围并关联原始日志。Windows 使用 `F:/tmp/<日期>/` 或 `E:/tmp/<日期>/`，不提交隔离 home、凭据或浏览器配置。

```markdown
# YYYY-MM-DD · 环境名称

- 版本/提交：
- 系统、硬件、权限：
- 启动介质与目标卷：
- 范围及未执行项目：

| ID | 调用与关键返回 | 独立核查 | 结论 | 耗时 |
|---|---|---|---|---|

### 缺陷与待查项

记录复现条件、预期/实际差异及后续状态。
```

历史实测通过与局部修复验证不能互相替代。

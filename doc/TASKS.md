# 待办与待决议题

本文集中记录全项目尚未完成的阶段开发、功能迭代、待决工程议题与实机生产验收任务。

## 主线阶段待办

完整阶段路线见 [Rust 重写开发计划](rust/plan/0.PLAN.md)。

| 阶段 | 模块 | 待完成内容 | 设计依据 |
|---|---|---|---|
| 阶段 9 | `desktop` | 补 Windows 前台输入、UAC、异构 DPI、远程桌面及桌面交互验收 | [9.desktop.md](rust/plan/9.desktop.md#开发步骤与验收方案) |
| 阶段 9.2 | `os` / `updater` / `kit` / `server` / `desktop` / `xtask` | 实现与本地发行包冒烟已完成。待发布前真机与签名验收 | [9.2.config-isolation-and-packaging.md](rust/plan/9.2.config-isolation-and-packaging.md) |
| 阶段 9.3 | `kit` / `client` / `updater` / `xtask` | 分三阶段推进性能优化与工程收敛：阶段 1 首要落地 `ls`/`disk` 重构与移除 `WizTree`（`1A–1E`），后续展开其余工具与全栈优化。进度见 [阶段 9.3 分阶段执行清单](#阶段-93-分阶段执行清单) | [9.3.optimization.md](rust/plan/9.3.optimization.md) · [ADR.md](ADR.md#adr-014移除-wiztree存储诊断改用纯-rust-mft-与-win32-ioctl) |
| 阶段 9.4 | `desktop` / `web` | 全应用方向键交互改造与动效优化（草案）：默认四角取景并在「设置 · 常规」提供「四角 / 游标」切换，配 `180ms` 平滑滑移动效，按 `P0–P2` 覆盖分支树、导出页、选择器、弹窗与设置控件 | [9.4.keyboard-navigation.md](rust/plan/9.4.keyboard-navigation.md) · [storybook.html](../design/storybook.html) |
| 阶段 10 | `desktop` | 在 `cst-pilot.exe` 同一窗口内集成基于 `Ratatui` 的内置 TUI 视图 `crates/cst-tui`，与图形视图共享 `cst-client` 状态层与草稿。评估以 `cst-tui` 替代 `tree-parity.test.ts` 的旧 Pi 对照基准（无可靠新参照前保留当前对照基准） | [0.PLAN.md](rust/plan/0.PLAN.md#10-内置-tui-终端适配) |
| 阶段 11 | `release` | 构建单文件便携引导器（`portable.exe`）与直观自有目录结构（根部仅保留主程序与清晰子目录，去除 `agent/home/` 等 Pi 遗留目录及过渡期 TS/`pack/` 资产），接入生产升级公钥 `CST_UPDATE_PUBLIC_KEY`，保持文档功能、签名推送更新与设计系统一致 | [0.PLAN.md](rust/plan/0.PLAN.md#11-便携打包与三端验收) · [ADR.md](ADR.md#adr-006阶段-11-采用单文件便携引导器与直观子目录分发) |

### 阶段 9.3 分阶段执行清单

详细方案与出口门禁见 [9.3.optimization.md](rust/plan/9.3.optimization.md)。验收勾选时附带测试版本、条件与结果或明确未覆盖项。

- [x] **前置准备**：完成 `ls`/`disk` 与 `WizTree` 调研、本机非提权隔离对照、[ADR-014](ADR.md#adr-014移除-wiztree存储诊断改用纯-rust-mft-与-win32-ioctl) 拍板及双人方案评审（实盘 MFT 未验，前移至 `1C` 硬门禁）
- **阶段 1（首要目标：`ls`、`disk` 与 Rust 发行移除 `WizTree`）**
  - [ ] **1A（代码、单测与本机 NVMe 实测已通过；待多控制器与慢速 SMB 补测）**：`2026-11-24` release 端到端 20 轮实测通过（`space` P95 `0.6 ms`、`health` 非提权 P95 `6.9 ms` / 提权 `10.2 ms`、`info(C:)` P95 `151.0 ms`、`all(C:)` P95 `118.7 ms`，原生路径免 WMI 达标，Storage WMI `MSFT_PhysicalDisk` 在本机约 84–151 ms）；未覆盖：SATA/USB-SAT/RST 控制器与断连 SMB 实测
  - [ ] **1B（代码、单测与本机 NVMe 红线实测已通过；待真实断连 UNC 补测）**：`2026-11-24` release 实测 `C:\Program Files` 热扫中位数 `393.4 ms`（达标 `≤ 500 ms`）、`System32` `167.6 ms`、`WinSxS`（24 万项）`12.37 s`、4 槽并发取消在 `32.1 ms` 退出且 `4.3 ms` 恢复可用；未覆盖：真实断连 UNC 槽位恢复
  - [ ] **1D（代码、单测与干净 `xtask dist` 发行包实测已通过；待真实旧版二进制交接）**：`2026-11-24` `xtask dist --profile release` 产出 42 文件（解包 `105.76 MiB` / ZIP `43.14 MiB`），零 `wiztree` 文件、`app/licenses/ntfs-ng-LICENSE.txt` 纳入 `SHA256SUMS` 且 ZIP/树校验与健康检查烟雾通过，更新器测试验证旧 WizTree 文件移除后逐字节回滚；未覆盖：已发布的旧版二进制原位升级交接
  - [ ] **1C（实验开关默认关闭；本机 512e NVMe 提权实盘门禁已通过，待 BitLocker 两态 / 4Kn / 热拔插补测后评估默认切流）**：`2026-11-24` 管理员提权 + `CST_ENABLE_MFT=1`（Samsung 970 EVO + WD SN580 512e NVMe，未加密卷）实测通过：`C:\`（212 万记录 / 165 万文件 / `214.80 GB`）冷扫 `4.61–5.91 s`、估算驻留 `58.26 MiB`、进程 Working Set `8.6 -> 97.5 MiB`（双卷 `107.3 MiB`，全周期峰值 `411.9 MiB`）；热 `ls`（`C:\` / `Windows` / `Program Files`）`2.2–22.5 ms` 均命中 `native-index`；`Program Files` 口径 `35.76 GB` vs `walk` `35.8 GB`；`D:\` 取消 `31.3 ms` 且紧邻复扫 `1.33 s` 直接成功；未覆盖：4Kn 盘、BitLocker 加密/锁定两态、热拔插换盘
  - [ ] **1E**：基于真实发布产物完成 Win10/11、WinPE、BitLocker、SATA/USB-SAT/RST、OneDrive、热拔插及三类只读布局验收
- **阶段 2（其余诊断与检索工具优化，阶段 1 封板后启动产品改造）**
  - [ ] **2A**：`fffind` / `ffgrep` 单次操作内身份复用降耗与 UTF-16 LE / GB18030 回退解码评估
  - [ ] **2B**：`sys(io)` `GetProcessIoCounters` 边界验证与 `sys(gpu)` 双显卡核显传感器保留
  - [ ] **2C**：联网检索并发容错（明确 `get_search_content` 范围）与 `runbook` / 提示词契约拆项收口
- **阶段 3（运行时、服务端与桌面端优化，量化后独立立项）**
  - [ ] **3A**：`pi-session` 内部结构收敛与 Web 前端全局事件订阅、大组件拆分
  - [ ] **3B**：桌面端首帧与长会话指标复测，同 `[profile.dist]` 分记 ZIP 与解包体积

## 功能迭代

| 模块 | 待办项 | 规格依据 |
|---|---|---|
| `web` | 兼容版：面向 Chromium 86 单独构建 Tailwind 3.4 样式并验证组件交互 | [web/SPEC.md](web/SPEC.md#浏览器基线与分发) |
| `web` / `desktop` | 仪表盘：实现 `/dashboard` 用量概览、趋势、筛选与逐请求本地日志，确定本地存储位置、保留期和清理方式 | [web/SPEC.md](web/SPEC.md#仪表盘) |
| `web` / `desktop` | 分享：复用会话导出接口，确定发送渠道、链接承载、敏感信息核对与销毁规则 | [web/SPEC.md](web/SPEC.md#命令与功能对应) |
| `web` | 会话内全文搜索与侧栏会话列表搜索 | [web/SPEC.md](web/SPEC.md#聊天工作台) |
| `web` | 视觉令牌微调（`P2`）：将 `.composer-chip .chip-remove` 热区放大至 `≥ 20×20px`，校准 `/sessions` 会话行高至 `40px`，对齐 `/login` 缩放标签与 `special-tool` 附属图标最小尺寸 | [ux-e2e.md](web/test/ux-e2e.md) |
| `telemetry` / `release` | 生产服务部署：上线 OA 请求级扣点接口 `/api/agent/llm/billing` 与版本升级签名清单接口 `/api/agent/releases/latest` | [telemetry/OPERATIONS.md](telemetry/OPERATIONS.md#发布与回退)、[release/SPEC.md](release/SPEC.md#rust-软件内升级) |
| `pi-core` | 内部结构优化（非阶段 9 阻塞）：将 `pi-session` 38 个闭包袋收敛为 `SessionHost` Trait，将 `AgentSessionInner` 80 个字段分组管理 | [8.5.pre-desktop-cleanup.md](rust/plan/8.5.pre-desktop-cleanup.md#明确不做) |
| `web` | 前端结构优化（非阶段 9 阻塞）：收敛 6 处重复全局事件订阅至 `WorkspaceStore`，拆分 `composer.tsx`、`shell.tsx` 与 `routes/tree.tsx` 大组件 | [8.5.pre-desktop-cleanup.md](rust/plan/8.5.pre-desktop-cleanup.md#明确不做) |

## 文档与设计规范待办

| 文档 / 资产 | 待办项 | 依据 |
|---|---|---|
| [cst-pilot-web.pen](../design/cst-pilot-web.pen) | Web 设计规范对齐：单文件画布合并（原 `cst-pilot-tools.pen` 已归档至 `design/achieved/`）、50 项静态变量、`mode: [light, dark]` 轴、`LKIQI` 规范板、`00A–00F`/`00H` 母版与 `01–07` 单行主区已完成；待同步迁移 Web 前端样式、缺口功能与 UX 契约测试 | [DESIGN.md](../DESIGN.md) · [storybook.html](../design/storybook.html) · [cst-pilot-desktop.pen](../design/cst-pilot-desktop.pen) |

## 待决议题

### 工程与安全边界

| 编号 | 议题 | 决策焦点 |
|---|---|---|
| E2 | BitLocker AI 填参的客户端草稿与附件脱敏边界 | 服务端执行日志与会话 JSONL 已由 `SessionDataPolicy` 脱敏。需确定前端本地草稿（`sessionStorage`）与图片附件中出现恢复密码时的检测与清理规则 |
| E4 | 收件箱结果未确认条目的后续处理动作 | 进程强杀遗留 `delivering` 条目并标为 `uncertain` 后，界面需提供明确的核对与清理动作，避免用户误操作触发重复推理 |
| E5 | 会话分享发布与隐私核对规则 | 确定分享链接的服务端承载、访问控制与过期销毁机制，明确机主信息与截图在上传前的确认流程 |

### 遥测数据与运维

| 编号 | 议题 | 决策焦点 |
|---|---|---|
| S1 | 会话效果评估指标 | 现有活动时长与取消次数仅反映运行耗时，评估是否补充会话内重复提问或界面等待等效果指标 |
| S4 | 模型报错原文保留上限 | 当前按出现次数降序最多保留 10 组完整报错原文，评估是否调整组数上限或增加错误类别计数 |
| O1 | 安装密钥遗失与异常批量注册治理 | 匿名安装签名仅证明持有本地私钥。需确定密钥丢失后的处理流程与异常批量注册告警阈值 |
| O2 | 按日汇总 CSV 导出与成员姓名核对 | 接收端 CLI 目前仅提供会话明细导出，需补充 `daily_rollup` 汇总导出及成员资料同步核对 |

## 实机与生产验证待办

验证方法与判定标准见 [TESTING.md](test/TESTING.md)。

| 范围 | 待验证内容 |
|---|---|
| 便携发行包与在线升级 | 干净发行副本在真实 U 盘（FAT32 / exFAT / NTFS）、只读或写保护介质、杀毒软件环境下的启动、诊断、原位签名升级、健康检查失败自动回滚与拔盘中断恢复 |
| 管理员提权与单实例 | 真实 UAC 同意／取消、普通账户输入管理员凭据跨账户提权、重复双击唤醒已有桌面窗口及双进程安全退出 |
| 桌面端与内置 TUI | Windows 10/11 多显示器异构 DPI 缩放、远程桌面、核显／无硬件加速回退、中文输入法候选框跟随、长会话滚动与窗口内 GUI/TUI 无缝切换 |
| Web 目标环境 | Chromium 111 基线、低端显卡、高刷屏、中文输入法组合输入、剪贴板粘贴与资源管理器拖入图片、33+ 标签页 `SharedWorker` 分片及宿主强杀恢复 |
| BitLocker 真机 | Windows 10/11 与 WinPE 下的隔离加密卷：只读状态、正确与错误恢复密码解锁、锁定卷拒绝解密（`0x80310000`）、系统卷自动解锁依赖（`0x80310029`）、后台解密完成判据与强杀重启复查 |
| CSTOA 生产服务 | 首次设备扫码与动态码授权、生产 APIKEY、多成员资料与额度点核对、真实请求级账单扣点核对 |
| 遥测生产与灾备 | 多队员授权后的双端一致入库、Windows 直接重启或强杀后的草稿恢复、双端数据库异机备份与恢复演练 |

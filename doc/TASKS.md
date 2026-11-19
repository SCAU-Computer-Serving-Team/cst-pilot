# 待办与待决议题

本文集中记录全项目尚未完成的阶段开发、功能迭代、待决工程议题与实机生产验收任务。

## 主线阶段待办

完整阶段路线见 [Rust 重写开发计划](rust/plan/0.PLAN.md)。

| 阶段 | 模块 | 待完成内容 | 设计依据 |
|---|---|---|---|
| 阶段 9 | `desktop` / `server` / `diagnostics` | 将 `crates/cst-pilot` 更名为 `crates/cst-kit`，实现 `crates/cst-client` 与 GPUI 桌面端 `crates/cst-desktop`（产出 `cst-pilot.exe`），将 `desktop-settings.json` 升级为 `desktop.json` 并接入 `.state/ui-hints.json` 与 `/api/app/update/*` | [9.desktop.md](rust/plan/9.desktop.md) |
| 阶段 10 | `desktop` | 在 `cst-pilot.exe` 同一窗口内集成基于 `Ratatui` 的内置 TUI 视图 `crates/cst-tui`，与图形视图共享 `cst-client` 状态层与草稿 | [0.PLAN.md](rust/plan/0.PLAN.md#10-内置-tui-终端适配) |
| 阶段 11 | `release` | 实现纯 Rust 便携发行装配与签名发布管线，接入生产升级公钥 `CST_UPDATE_PUBLIC_KEY`，移除过渡期 TypeScript 扩展与旧版 `pack/` 运行时装配 | [0.PLAN.md](rust/plan/0.PLAN.md#11-便携打包与三端验收) |

## 功能迭代

| 模块 | 待办项 | 规格依据 |
|---|---|---|
| `web` | 兼容版：面向 Chromium 86 单独构建 Tailwind 3.4 样式并验证组件交互 | [web/SPEC.md](web/SPEC.md#浏览器基线与分发) |
| `web` / `desktop` | 仪表盘：实现 `/dashboard` 用量概览、趋势、筛选与逐请求本地日志，确定本地存储位置、保留期和清理方式 | [web/SPEC.md](web/SPEC.md#仪表盘) |
| `web` / `desktop` | 分享：复用会话导出接口，确定发送渠道、链接承载、敏感信息核对与销毁规则 | [web/SPEC.md](web/SPEC.md#命令与功能对应) |
| `web` | 会话内全文搜索与侧栏会话列表搜索 | [web/SPEC.md](web/SPEC.md#聊天工作台) |
| `web` | 视觉令牌微调（`P2`）：将 `.composer-chip .chip-remove` 热区放大至 `≥ 20×20px`，校准 `/sessions` 会话行高至 `40px`，对齐 `/login` 缩放标签与 `special-tool` 附属图标最小尺寸 | [ux-e2e.md](web/test/ux-e2e.md) |
| `telemetry` / `release` | 生产服务部署：上线 OA 请求级扣点接口 `/api/agent/llm/billing` 与版本升级签名清单接口 `/api/agent/releases/latest` | [telemetry/OPERATIONS.md](telemetry/OPERATIONS.md#发布与回退)、[release/SPEC.md](release/SPEC.md#rust-软件内升级) |

## 文档与设计规范待办

| 文档 / 资产 | 待办项 | 依据 |
|---|---|---|
| [cst-pilot-web.pen](../design/cst-pilot-web.pen) / [cst-pilot-tools.pen](../design/cst-pilot-tools.pen) | Web 设计规范对齐：按设计核心文件体系对齐静态画布、Shader 绑定、令牌与交互行为，导入 `mode: [light, dark]` 变量轴与 `00A–00F` 可复用母版，重排主区为 `y = 0` 单行线性流并同步迁移 Web 前端样式与契约测试 | [DESIGN.md](../DESIGN.md) · [storybook.html](../design/storybook.html) · [cst-pilot-desktop.pen](../design/cst-pilot-desktop.pen) |

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

验证方法与判定标准见 [TESTING.md](TESTING.md)。

| 范围 | 待验证内容 |
|---|---|
| 便携发行包与在线升级 | 干净发行副本在真实 U 盘（FAT32 / exFAT / NTFS）、只读或写保护介质、杀毒软件环境下的启动、诊断、原位签名升级、健康检查失败自动回滚与拔盘中断恢复 |
| 管理员提权与单实例 | 真实 UAC 同意／取消、普通账户输入管理员凭据跨账户提权、重复双击唤醒已有桌面窗口及双进程安全退出 |
| 桌面端与内置 TUI | Windows 10/11 多显示器异构 DPI 缩放、远程桌面、核显／无硬件加速回退、中文输入法候选框跟随、长会话滚动与窗口内 GUI/TUI 无缝切换 |
| Web 目标环境 | Chromium 111 基线、低端显卡、高刷屏、中文输入法组合输入、剪贴板粘贴与资源管理器拖入图片、33+ 标签页 `SharedWorker` 分片及宿主强杀恢复 |
| BitLocker 真机 | Windows 10/11 与 WinPE 下的隔离加密卷：只读状态、正确与错误恢复密码解锁、锁定卷拒绝解密（`0x80310000`）、系统卷自动解锁依赖（`0x80310029`）、后台解密完成判据与强杀重启复查 |
| CSTOA 生产服务 | 首次设备扫码与动态码授权、生产 APIKEY、多成员资料与额度点核对、真实请求级账单扣点核对 |
| 遥测生产与灾备 | 多队员授权后的双端一致入库、Windows 直接重启或强杀后的草稿恢复、双端数据库异机备份与恢复演练 |

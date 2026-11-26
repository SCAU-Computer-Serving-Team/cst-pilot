---
name: runbook
description: runbook 手动修复命令方案与导出规范。创建或修订修复方案（plan）、划分 safe / low / confirm 风险档、编写单行命令或导出 txt 清单（export）时查阅。
---

# runbook

唯一的手动修复命令方案工具。保存版本化方案至 `data/plans/`，按需导出纯文本清单至 `artifacts/<日期>/runbook/`，不执行任何命令，不改动机主系统。

## 允许用途与禁区

- **允许**：队员必须亲手执行的修复动作，如清理系统缓存、卸载软件、关闭自启项、重装或移除驱动、重置网络配置。
- **禁止写入诊断采集**：收集系统与磁盘信息一律调用诊断工具。
- **禁止写入不可逆高危操作**：格式化、`diskpart clean`、改写引导、卸载系统关键驱动、批量删除注册表项。这类操作只在对话里说明风险，由队员自行判断。

## 执行流程与完成判据

1. 按风险档拆批：同批、同风险档的命令合并成一个方案；不同风险档分别调用 `runbook` 保存并写明执行顺序。
2. 创建方案：调用 `runbook`（`action: "plan"`，可省略），传入 `title`、`level` 与完整 `items`。默认保持 `exportTxt: false`，队员可直接在界面卡片核对并一键导出打开记事本，不在正文重复粘贴整段命令。
3. 修订方案：调用 `runbook`（`action: "plan"`），必须成对传入原 `runbookId` 与当前最新 `baseVersion` 及完整 `items`（禁止传 `version`）。同方案 `sequence` 保持不变，`version` 递增 1。
4. 处理冲突：修订报版本冲突时，先用 `read` 读取 `data/plans/<runbookId>/v<最新版本>.json` 核对当前命令，再带最新 `baseVersion` 修订。已导出的 `.txt` 若被现场修改，工具拒绝覆盖，须修订出新版本再导出。
5. 按需导出：仅在队员要求由助手导出 `.txt` 时，于 `plan` 中传 `exportTxt: true`，或单独调用 `runbook({ action: "export", runbookId, version })`（`version` 须为最新版本，禁传编辑字段）。
6. **完成判据**：说明权限要求、主要影响与执行顺序；已导出时原样转述 `file` 路径、版本号与条目数，并提醒纯文本不会自动执行、须逐条复制运行。队员已执行旧版部分条目时，明确指出本次改动与需跳过的条目。

## 风险分档

| 档位 | `level` | 判据 | 示例 |
|---|---|---|---|
| 安全 | `safe` | 只删系统可再生的缓存与临时数据，不动用户数据，不改配置，无需管理员 | 清空 `%TEMP%`、清理更新缓存、清空回收站 |
| 较安全 | `low` | 修改可回退的配置或用户可见状态，影响范围明确 | 卸载自装软件、关闭自启项、重置网络栈 |
| 需确认 | `confirm` | 涉及机主个人数据或系统核心行为，动手前须征得机主知情同意 | 删除大文件或重复文件、禁用系统服务、删除还原点 |

分档按操作的实际后果判定，例如清理 `%TEMP%` 归 `safe`，删除用户文档归 `confirm`。

## 命令、说明与环境写法

1. **单行无控制字符**：每条命令 1–2000 字，禁止换行；每方案 1–20 条。
2. **同类合并**：同一类、同一影响的操作合并成一条（如清理多个缓存目录），不同目的的操作分开写。
3. **不写脚本与注释**：不使用 `if`、变量赋值、函数定义，不写 `#` 或 `rem` 注释，说明统一放 `summary`。
4. **谨慎使用连接符**：`cmd` 用 `&&` 保证前步成功再继续，`&` 无条件继续；PowerShell 的 `;` 不会因前步失败自动中断，有依赖条件时拆条由队员核查执行。
5. **路径与服务恢复**：尽量避免中文，必须含中文路径时保持引号完整。清理过程中临时停止服务或进程须安排恢复，单行无法保证失败后恢复时拆成清晰分步。
6. **`summary` 规范**：1–300 字单行，动词开头，同时写清动作对象与实际影响（释放空间、需重启、可能报错、是否可恢复）。合格示例：`删除当前用户临时文件夹内容，释放 C 盘空间；使用中的文件可能报错跳过，删除后不可恢复。`
7. **`shell` 与 `admin`**：`shell` 只填小写 `cmd` 或 `powershell`。`admin` 默认 `false`；需管理员权限的命令必须设 `true` 并在 `summary` 写明「需要管理员权限」。

## 参数与返回字段

| 调用模式 | 参数约束 |
|---|---|
| `action: "plan"`（默认） | 必填 `title`（1–48 字单行）、`level`、`items[]`（`summary`、`command`、`shell`、`admin?`）；可选 `date`（`YYYY-MM-DD`，修订不可改）、`runbookId` + `baseVersion`（修订成对传）、`exportTxt`（默认 `false`）；禁止传 `version` |
| `action: "export"` | 仅接受 `runbookId` 与最新 `version`，禁止同传任何方案编辑字段 |

返回结构为 `{ "runbook": { ... }, "elapsedMs": N }`：

- 公共字段：`runbookId`、`sequence`（当天 `01–999`）、`version`、`day`、`createdAt`、`timeText`、`title`、`level`、`levelLabel`、`items`（条数）、`commands[]`、`action`（`planned` / `exported`）、`notice`。
- 导出附加字段：`file`、`dir`、`name`、`bytes`、`encoding`（`utf-8-bom`）。导出路径固定为 `artifacts/<YYYY-MM-DD>/runbook/<序号:02>-<风险档>-<标题>-v<版本>.txt`（UTF-8 BOM + CRLF）。
- 校验失败、版本冲突、序号达 999 上限或并发目录锁占用均返回 `isError: true`（`{"error", "elapsedMs"}`），按提示核对后重试。

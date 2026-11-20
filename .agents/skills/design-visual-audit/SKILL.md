---
name: design-visual-audit
description: Cross-verify implemented desktop UI against source design specs (.pen, storybook, design rules) with isolated real-window telemetry and dual-theme screenshots. Use when auditing UI Node IDs, fixing layout/spacing/theme mismatches, verifying streaming or error states, or gating a UI milestone with peer code review.
---

# 设计与真窗交叉核查（Design Visual Audit）

本技能规范桌面端界面实现与原设计材料（`.pen` 画布、交互规范与文本规范）的逐项交叉核查闭环。每次核查均执行「拆解规范 -> 隔离取证 -> 源码修正 -> 门禁与真窗复测 -> 本地提交 -> `sol` 审查直改与越界核验」流程。

## 执行步骤

1. **提取设计基准**
   - 同步调用设计画布工具读取目标 Node ID 及其引用母版的完整节点树与参考截图。
   - 对照交互文档确认瞬态、动效参数与焦点规则；对照设计总纲确认文案、术语与材质停帧边界。
   - **完成标准**：形成可核对的静态对照清单，逐一明确高度、内边距、元素间距、`4/8/12px` 圆角阶梯、描边、字号行高、图标尺寸、语义色与中文 `2px` 底部对齐槽。

2. **构建隔离实例并采集基线截图**
   - 所有过程产物（隔离 `$kitDir`、临时 `CARGO_TARGET_DIR`、Mock 脚本、Diff 快照与真窗截图）统一写入仓库外 `F:/tmp/<YYYY-MM-DD>/` 或 `E:/tmp/<YYYY-MM-DD>/`，禁止写入工作区或项目 `.gemini` 目录。
   - 按 [winapp-cli.md](../../doc/desktop/test/winapp-cli.md) 组装独立临时目录 `$kitDir`，设置 `PI_CODING_AGENT_DIR` 与 `CST_ALLOW_UNELEVATED=1`，由 `--kit-root` 启动服务与桌面双进程。
   - 按核查范围注入测试夹具：历史对话与异常态写入 `sessions/*.jsonl`，深浅主题修改 `desktop.json`，实时流式输出将 `models.json` 指向本机临时 Mock SSE 服务。
   - 使用 `winapp ui inspect --interactive --json` 解析控件 ID，全部操作走后台 UIA 通道与 `post-message`，保持宿主机焦点与键鼠不受干扰。
   - **完成标准**：取得浅色、深色及对应展开/报错/流式真窗基线截图，并逐张识图列出源码偏差。

3. **修正实现并覆盖底层默认值**
   - 在桌面端视图源码中集中修改对应节点，同步检查底层控件（如 `TextViewStyle`、`Textarea`）的默认内边距、圆角、阴影与高亮色，显式覆写深色主题下的遗留浅色值。
   - 修改过程中保持现有测试 `AutomationId` 命名不变。
   - **完成标准**：全部已定位偏差修正完毕，代码保持单一职责与复用约束。

4. **通过双特性门禁与产物构建**
   - 在独立 `CARGO_TARGET_DIR` 下依次执行 `cargo fmt --all`、默认与 `--features test-support` 两套 `cargo clippy`（`-D warnings`）、目标集成测试，以及可执行文件重编。
   - **完成标准**：格式检查、双特性 Clippy 与测试全部零错误零跳过通过，并产出最新二进制文件。

5. **双主题真窗复测与移交前提交**
   - 使用最新构建产物覆盖 `$kitDir` 中的二进制文件，重新采集浅色、深色、展开、报错及流式全态真窗截图并逐张识图核对。
   - 关闭所有临时服务与模拟进程，核对仓库文件状态无临时产物残留；因 UIA 能力限制（如未暴露 `ScrollPattern`）而未能真窗触发的子态，必须在测试文档中写明未覆盖边界。
   - 门禁与真窗复测均通过后，将本大项改动在本地 `git commit`，保留干净提交基线用于隔离同行修改。
   - **完成标准**：全部基线偏差在真窗截图上确认消弭，现场清理干净，形成可独立对比的本地基准提交。

6. **新建 `sol` 直接修bug与越界复核**
   - 每次新建独立的交互式 `pi` 终端（`hc run --bg --name review-<slug> --cwd <repo> pi --provider openai-codex --model gpt-6.2-sol --thinking xhigh`），`hc wait` 就绪后通过 `hc send` 传入本大项基准提交范围与审查指令。
   - 指示 `sol` 审查逻辑错误、边界缺陷、状态与生命周期、资源泄漏及工程规范，直接在工作区修复问题并独立创建本地提交；明确要求其不动视觉设计、设计令牌与无障碍 `AutomationId`，不派生子代理，完成后用 `hc send $HC_TERMINAL_ID` 回传提交哈希与修改摘要。
   - 收到回传后，基于基准提交与 `sol` 提交的 `git diff` 执行逐行核查与合理质疑：
     1. 核对是否越界修改设计规格、令牌、视觉尺寸或无障碍 `AutomationId`；
     2. 独立复核修改理由在业务逻辑、GPUI 渲染机制与焦点/交互语义上是否真实成立；
     3. 发现误判、过度重构或副作用时直接回退调整，保留合理修复后重跑格式、双特性 Clippy、相关测试及必要截图，随后关闭临时 `hc` 终端（`hc kill`）。
   - 收口当前阶段性任务：更新核验清单状态与差距说明，并在本地自动执行一次 `git commit` 留存阶段节点标记（遵循 `类型(范围): 描述` 提交格式，仅本地提交，不自动推送远端）。
   - **完成标准**：`sol` 提交经逐行质疑与越界核查后无问题，重跑门禁与必要截图通过，核验清单已更新并完成阶段性本地提交。

## 高频避坑速查

| 阶段 | 常见失效现象 | 必检核对项 |
|---|---|---|
| 样式覆写 | 深色模式行内代码成纯白块，或输入框偏高 | 显式设置 `HighlightStyle`、`inline_code_style`、`rounded` 及输入框最小内边距 |
| 中文对齐 | 单行文本或紧凑按钮文字视觉偏上 | 与图标同行居中的中文标签统一补 `2px` 底部光学偏移 |
| 隔离启动 | 仅拷 `.exe` 弹出 `#32770` 系统报错框 | 补齐 `APPEND_SYSTEM.md`、`skills/*`、`VERSION` 与 `--kit-root` 参数 |
| 主题夹具 | 修改 `settings.json` 无法切换桌面端外观 | 桌面端外观读取 `agent/home/desktop.json` 中的 `theme` 字段 |
| 控件解析 | 正则提取控制台文本 ID 混入 ANSI 控制符 | 解析控件列表统一附加 `--json` 参数 |
| 构建同步 | 验证目录仍运行未更新的旧版 `.exe` | 重编后第一时间将最新二进制拷入 `$kitDir` 再截图 |

## 外部参考

- 桌面端自动化命令与隔离环境模板：[winapp-cli.md](../../doc/desktop/test/winapp-cli.md)
- 项目当前核验进度与清单总表：[desktop-ui-audit.md](../../doc/test/desktop-ui-audit.md)
- 文本红线与材质保护约束：[DESIGN.md](../../DESIGN.md)
- 交互瞬态、动效曲线与 L0–L4 材质规范：[storybook.html](../../design/storybook.html)

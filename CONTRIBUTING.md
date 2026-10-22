# 贡献规范

本文说明本地开发、检查与提交流程。项目结构见 [README](README.md)，开发约定见 [AGENTS.md](AGENTS.md)，产品与设计文档见 [doc/](doc)。

## 环境

Windows 上开发，命令用 PowerShell 执行。需要 Node 22.19.0 以上，仓库自带 `node/`（不入库）时可以改用它。字体生成使用 Python 3.11，与 CI 保持一致。

首次准备依赖：

```powershell
npm ci --prefix agent --ignore-scripts
npm ci --prefix src/web/frontend --ignore-scripts
python -m pip install -r src/web/frontend/scripts/font-requirements.txt
```

`agent` 是扩展与工具的后端，`src/web/frontend` 是 Web 前端源码。两处依赖各自独立。Python 与 FontTools 仅用于开发和 CI 的字体生成，不进入现场运行链；详见[字体分发](doc/design/web/fonts.md)。

## 检查与测试

| 范围 | 命令 | 说明 |
|---|---|---|
| 脚本、配置与 CSS | `npm run check:engineering --prefix agent` | Biome 静态规则与 PowerShell 语法 / PSScriptAnalyzer 1.24.0；覆盖已跟踪和新增文件 |
| 后端格式与类型 | `npm run check --prefix agent` | biome 检查加 `tsc` |
| 后端测试 | `npm run test:web --prefix agent` | Web 扩展的接口、会话与事件测试 |
| 跨端共享验收 | `npm run test:web:shared-audit --prefix agent` | 独立 Pi 进程、真实工具、写权、上下文、配置与独立主题；CI 必跑 |
| OAuth 测试 | `npm run test:oauth --prefix agent` | 模拟 OA 与真实 Pi 认证链路 |
| 遥测测试 | `npm run test:telemetry --prefix agent` | 双端队列、独立确认与 TLS 故障；TLS 测试需要 OpenSSL |
| 发行脚本测试 | `node --test pack/test/*.test.mjs` | 启动器、发行树、模型目录与遥测配置 |
| 前端格式与规则 | `npm run lint --prefix src/web/frontend` | biome 检查，规则与配置在仓库根 `biome.json` |
| 前端类型 | `npm run typecheck --prefix src/web/frontend` | react-router 类型生成加 `tsc` |
| 前端测试 | `npm run test --prefix src/web/frontend` | 工具卡片映射、分支树与流式渲染的纯函数测试 |
| 工具与文档约束 | `node --test src/test/*.test.mjs` | 诊断工具、凭据边界、画布颜色与节点完整性、本地文档链接 |
| 前端构建 | `npm run build --prefix src/web/frontend` | 产物写入 `agent/home/extensions/web/static/` |
| 分发字体 | `npm run test:fonts --prefix src/web/frontend` | 构建后检查覆盖、hash、回退与体积 |
| 端到端验收 | `npm run test:web:e2e --prefix agent` | 先构建；真实浏览器、HTTP、Pi、持久化与重启，链路见[验收设计](doc/test/web-e2e.md) |

前端 lint 必须从 `src/web/frontend` 目录发起：biome 按运行目录识别 React 项目，换目录会漏掉 React 相关规则。`npm --prefix` 已经处理，直接照表执行即可。

工程检查使用已安装的 PSScriptAnalyzer 1.24.0，也可用 `CST_PSSCRIPTANALYZER_MODULE` 指向该版本的模块清单；CI 安装固定版本。MJS、JSON、CSS 启用静态规则，不批量格式化已有文件；CSS 的层叠优先级和 `!important` 由界面回归验证，重复属性、非法语法等仍检查。

`pi.cmd` 交互启动请求管理员授权，优先使用已安装的 Windows Terminal。管道调用与发行冒烟需由已提权终端发起；启动器测试使用隔离副本和授权替身，不自动弹出 UAC。

提交前运行与改动相关的检查。修改后端运行后端检查与测试；修改前端或画布运行前端格式、类型、测试、设计约束和构建。CI 覆盖上表，见 [.github/workflows/ci.yml](.github/workflows/ci.yml)。

## 提交

提交信息用 `类型(范围): 描述`，例如 `fix(web): 会话切换不再打断生成`、`docs(Todo): 补画布对照项`。类型取 `feat`、`fix`、`docs`、`chore`、`design`、`test` 之一，范围可省略。

改动的代码与文档同步提交：行为变了就更新对应 SPEC，待办完成了就更新 [doc/Todo.md](doc/Todo.md)。

不要提交密钥与运行态。`.gitignore` 已覆盖 `auth.json`、`models.json`、`web-settings.json`、`sessions/`、`web-access.log` 与便携运行时目录；新出现的运行产物也要一并加进去。

## 提交到上游

直接向 `main` 提交，或开 PR 由 CI 验证后合并。CI 覆盖后端检查、后端测试、发行脚本测试，以及前端的 lint、类型、测试与构建；任一项失败都不合并。

## 报告问题

用仓库的 issue 模板：

| 模板 | 用途 |
|---|---|
| 缺陷报告 | 功能没按预期工作，需要复现步骤与实际结果 |
| 议题讨论 | 需求、方案取舍、文档口径等待讨论的问题 |

密钥、机主隐私数据、真实会话内容不要贴进 issue。

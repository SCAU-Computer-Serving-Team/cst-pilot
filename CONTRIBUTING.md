# 贡献规范

本文说明本地开发、检查与提交流程。项目结构见 [README](README.md)，开发约定见 [AGENTS.md](AGENTS.md)，产品与设计文档见 [doc/](doc)。

## 环境

Windows 上开发，命令用 PowerShell 执行。需要 Node 22 以上，仓库自带 `node/`（不入库）时可以改用它。

首次准备依赖：

```powershell
npm ci --prefix agent
npm ci --prefix src/web/frontend
python -m pip install -r src/web/frontend/scripts/font-requirements.txt
```

`agent` 是扩展与工具的后端，`src/web/frontend` 是 Web 前端源码。两处依赖各自独立。Python 与 FontTools 仅用于开发和 CI 的字体生成，不进入现场运行链；详见[字体分发](doc/design/web/fonts.md)。

## 检查与测试

| 范围 | 命令 | 说明 |
|---|---|---|
| 后端格式与类型 | `npm run check --prefix agent` | biome 检查加 `tsc` |
| 后端测试 | `npm run test:web --prefix agent` | Web 扩展的接口、会话与事件测试 |
| 发行脚本测试 | `node --test pack/test/*.test.mjs` | 发行树校验与模型目录 |
| 前端格式与规则 | `npm run lint --prefix src/web/frontend` | biome 检查，规则与配置在仓库根 `biome.json` |
| 前端类型 | `npm run typecheck --prefix src/web/frontend` | react-router 类型生成加 `tsc` |
| 前端测试 | `npm run test --prefix src/web/frontend` | 工具卡片映射、分支树与流式渲染的纯函数测试 |
| 画布与设计约束 | `node --test src/test/*.test.mjs` | 颜色、节点 ID、配置页布局与动效令牌 |
| 前端构建 | `npm run build --prefix src/web/frontend` | 产物写入 `agent/home/extensions/web/static/` |
| 分发字体 | `npm run test:fonts --prefix src/web/frontend` | 构建后检查覆盖、hash、回退与体积 |
| 端到端验收 | `npm run test:web:e2e --prefix agent` | 先构建；真实浏览器、HTTP、Pi、持久化与重启，链路见[验收设计](doc/test/web-e2e.md) |

前端 lint 必须从 `src/web/frontend` 目录发起：biome 按运行目录识别 React 项目，换目录会漏掉 React 相关规则。`npm --prefix` 已经处理，直接照表执行即可。

提交前运行与改动相关的检查。修改后端运行后端检查与测试；修改前端或画布运行前端格式、类型、测试、设计约束和构建。CI 覆盖上表，见 [.github/workflows/ci.yml](.github/workflows/ci.yml)。

## 提交

提交信息用 `类型(范围): 描述`，例如 `fix(web): 会话切换不再打断生成`、`docs(Todo): 补画布对照项`。类型取 `feat`、`fix`、`docs`、`chore`、`design`、`test` 之一，范围可省略。

改动的代码与文档同步提交：行为变了就更新对应 SPEC，待办完成了就更新 [doc/Todo.md](doc/Todo.md)。

不要提交密钥与运行态。`.gitignore` 已覆盖 `auth.json`、`models.json`、`sessions/`、`web-access.log` 与便携运行时目录；新出现的运行产物也要一并加进去。

## 提交到上游

直接向 `main` 提交，或开 PR 由 CI 验证后合并。CI 覆盖后端检查、后端测试、发行脚本测试，以及前端的 lint、类型、测试与构建；任一项失败都不合并。

## 报告问题

用仓库的 issue 模板：

| 模板 | 用途 |
|---|---|
| 缺陷报告 | 功能没按预期工作，需要复现步骤与实际结果 |
| 议题讨论 | 需求、方案取舍、文档口径等待讨论的问题 |

密钥、机主隐私数据、真实会话内容不要贴进 issue。

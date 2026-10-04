# Python 环境盘点设计

状态：已实现。需求：[Python 环境专项清理](https://github.com/SCAU-Computer-Serving-Team/cst-pilot/issues/1)。

## 目标

旧工具链只回答 CPU、磁盘、驱动和事件日志，无法解释“Python 到处装、pip 装错环境、Conda 与 Miniconda 共存”。新增只读 `python` 工具，先把环境事实和冲突说清楚，再交给 `runbook` 生成人工确认后的清理命令。

## 为什么只读

Python 环境目录可能包含项目代码、固定版本解释器、编译器、CUDA、PyMOL 插件和用户数据。仅凭路径和最后访问时间不能判断可删除性。自动删除会把“看起来重复”的环境变成不可恢复的数据损失。

因此 `python` 只返回：

1. 命令入口和版本。
2. Conda 根目录、环境目录和配置。
3. PATH、`VIRTUAL_ENV`、`CONDA_PREFIX` 等活跃环境。
4. 冲突和待核查清单。

真正的删除由 `runbook` 生成命令，用户逐项确认后执行。

## 采集边界

- `Get-Command` 只取 PATH 上可见的入口。
- Conda 使用 `conda info --json`，不修改 `.condarc`。
- 目录扫描只覆盖当前项目、当前激活环境和已知根目录，默认深度 3。
- `includeSizes=true` 才递归统计环境体积。
- 不遍历整个磁盘，不读取环境中的包内容。

## 冲突判断

| 判断 | 含义 |
|---|---|
| `multiple-python-commands` | PATH 或启动器解析到多个不同 Python，pip 目标可能错位 |
| `multiple-package-managers` | Conda/Mamba/uv/Poetry/pipx/virtualenv 等职责重叠 |
| `anaconda-and-miniconda` | 两个 Conda 根目录重复占空间并可能争抢 PATH |
| `active-environment-not-found` | 当前环境变量指向已删除或不可读目录 |
| `pymol-in-conda` | PyMOL 落在 Conda 根或大环境，安装体量可能被连带放大 |
| `venv-and-conda-active` | 同一 shell 同时存在两个活跃环境标记 |

判断只说明风险，不自动选择“保留哪个”。选择依据是项目、Python 版本、二进制依赖和用户工作流。

## 清理工作流

1. `python scope=overview` 判断安装入口冲突。
2. `python scope=environments includeSizes=true` 获取环境清单和体积。
3. 对每个待删环境确认项目、解释器、包依赖和重建方式。
4. 对确认安全的环境调用 `runbook`，按环境生成一条带说明和影响范围的命令。
5. 不删除当前激活环境、当前项目环境、PyMOL/编译工具链依赖环境和来源不明的环境。

## 验证

1. 在仅有系统 Python 的机器上，返回单个安装入口，不报错。
2. 在 Conda + venv 混用机器上，返回两个活跃标记并给出冲突。
3. `includeSizes=false` 时 `bytes` 为 `null`，不触发递归统计。
4. 扫描深度不超过 `maxDepth`，不访问盘符根目录。
5. 所有分支只发送只读命令；工具注册后不影响现有七个工具。

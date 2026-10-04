---
name: python
description: 盘点 Python、Conda、Miniconda、虚拟环境和包管理入口；定位 pip 装错环境、环境重复和 PyMOL 连带安装问题的只读流程。
---

# Python 环境盘点

只读工具 `python`。用于回答“电脑上装了几套 Python”“pip 为什么装到别处”“Conda 和 Miniconda 能不能清”“PyMOL 为什么拖进完整环境”。

## 顺序

1. 先调用 `python({ scope: "overview" })`。
2. 需要看清全部入口时调用 `scope=installations`。
3. 需要清理清单时调用 `scope=environments`，环境很大再加 `includeSizes: true`。
4. 需要检查 Conda、pipx、Poetry、uv 配置时调用 `scope=config`。

## 判断

| 现象 | 先看 | 结论边界 |
|---|---|---|
| `pip install` 后模块找不到 | `commands`、`path`、`environmentVariables` | 确认是 PATH、pip 目标、虚拟环境还是 PyPI 包名问题 |
| Conda 与 Miniconda 共存 | `installations`、`conda.root_prefix`、`conda.envs_dirs` | 只说明重复；保留哪套要结合项目和体积 |
| PyMOL 安装体量异常 | `commands.pymol.path`、`issues` | 只说明 PyMOL 落在 Conda；不自动重建 |
| 项目环境到处散落 | `environments`、`cleanupReview` | 未激活不等于可删除 |
| 多个环境标记同时存在 | `VIRTUAL_ENV`、`CONDA_PREFIX` | 冲突只影响当前 shell，不代表磁盘坏 |

## 清理

禁止直接删除环境。只有用户确认项目、解释器和依赖不再使用后，才调用 `runbook` 生成命令。

清理项按风险分档：

| 档 | 对象 | 处理 |
|---|---|---|
| 不删 | 当前 `VIRTUAL_ENV`、`CONDA_PREFIX`、当前项目环境、PyMOL/编译链依赖环境 | 保留 |
| 先查 | 已知管理器的 inactive 环境、重复 Conda 根目录、孤立 `venv` | 确认后生成命令 |
| 只给信息 | 来源不明的目录、权限不足的环境、PATH 中的残留项 | 提示用户人工判断 |

每条 `runbook` 命令必须写清环境、说明、命令正文和影响范围。不要自动删除 `.condarc`、pip 缓存、项目源码或用户数据。

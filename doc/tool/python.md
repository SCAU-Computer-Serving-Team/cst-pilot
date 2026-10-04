# python

只读盘点 Windows 上的 Python 安装、虚拟环境、Conda/miniconda 根目录和包管理入口。用于定位“pip 装错地方”“Conda 与 Miniconda 共存”“PyMOL 拉进完整 Conda 环境”“项目虚拟环境散落”等问题。

实现：[python.ts](../../agent/home/extensions/diagnostics/python.ts)、[python-core.ts](../../agent/home/extensions/diagnostics/python-core.ts)

## 调用

`python({ scope, path?, maxDepth?, includeSizes? })`

| 参数 | 必填 | 说明 |
|---|---:|---|
| `scope` | 是 | `overview`、`installations`、`environments`、`config`、`all` |
| `path` | 否 | 额外扫描的项目目录；省略时扫描当前工作目录 |
| `maxDepth` | 否 | 项目目录扫描深度，默认 3，范围 1～5 |
| `includeSizes` | 否 | 是否递归计算环境占用，默认 `false`。大环境可能增加数秒耗时 |

## scope

| scope | 返回 |
|---|---|
| `overview` | 命令与路径、环境变量、安装入口、冲突判断 |
| `installations` | Python、`py`、Conda、Mamba、uv、Poetry、pipx、virtualenv 等入口 |
| `environments` | 已发现的虚拟环境、Conda 环境、活跃状态、可选大小、清理待核查清单 |
| `config` | Conda、pipx、Poetry、Pyenv、uv 的配置输出和环境变量 |
| `all` | 上述全部 |

## 返回

`python.commands`：每个命令入口的 `name`、`path`、`type`、`version`。同名命令可能有多个路径，重复项不合并。

`python.installations`：按命令入口和 Conda 根目录归一化后的安装清单。

`python.environments`：环境目录、类型、来源、版本、是否当前激活和可选占用。来源可能为 `VIRTUAL_ENV`、`CONDA_PREFIX`、`conda`、`poetry`、`scan-path` 或 `known-root`。

`python.issues`：只读判断，不等同于修复方案。常见码：

- `multiple-python-commands`
- `multiple-package-managers`
- `anaconda-and-miniconda`
- `active-environment-not-found`
- `pymol-in-conda`
- `venv-and-conda-active`

`python.cleanupReview`：未被当前 `VIRTUAL_ENV`/`CONDA_PREFIX` 选中的环境。只是待核查清单。删除前必须确认项目、解释器和包依赖仍在使用。

## 边界

- 工具不执行 `pip uninstall`、`conda remove`、`rmdir` 或任何写操作。
- 不完整遍历整个磁盘；只查 PATH、当前环境、Conda/Poetry 配置和已知根目录。
- `includeSizes=false` 时 `bytes` 为 `null`。
- 无法读取的目录会保留在结果中，并在 `degraded`/`collectionErrors` 或 `notice` 中说明。
- 环境清理必须由用户确认，然后用 `runbook` 生成逐项命令。

## 示例

快速诊断：

```text
python({ "scope": "overview" })
```

盘点某项目的环境：

```text
python({ "scope": "environments", "path": "E:\\Learning\\project", "maxDepth": 4 })
```

清理前估算占用：

```text
python({ "scope": "environments", "includeSizes": true })
```

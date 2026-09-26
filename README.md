# CST Pilot

> Computer Service Team · Portable Diagnostics Kit

<p align="center">
  <img src="assets/logo.png" alt="CST Pilot" width="820">
</p>

<p align="center">
  <b>下载</b>：<a href="https://github.com/SCAU-CST/cst-pilot/releases/latest">最新发行版</a> · <a href="https://github.com/SCAU-CST/cst-pilot/releases">全部版本</a>
</p>

**CST Pilot** 是计算机维护队（Computer Service Team, **CST**）的便携式专用Agent。

特点：

1. **基于热门开源项目**。基于Pi改的定制Agent，专门用于给计维队员提供便捷的AI技术支持。
2. **充足的定制**。定制Agent有专门的提示词和SKILLS。并且有专用的工具获取磁盘状态，进程占用状态等。
3. **安全**：默认只向模型开放读取、检索与诊断工具。高度可控。
4. **高效**：使用 WizTree 等第三方工具加速磁盘扫描
5. **即插即用**。本项目发行版可存放在U盘，内置所有所需的环境。不必在机主电脑上安装任何东西。

## 功能

### 当前已实现

1. 扫描机主的磁盘状态，分析C盘等磁盘的文件占用情况，AI分析后可给出清理建议
2. 分析机主当前进程的运行状态，给出占用CPU，内存，GPU等状态
3. 获取机主硬件参数，以及温度、风扇、电压、降频等
4. 整机负载概况：物理内存、CPU 总占用率、页面文件、开机时长
5. 开机自启盘点：注册表 Run 键（含任务管理器禁用状态）、启动文件夹、自启服务
6. 获取Windows事件日志：最近错误/警告、开关机·蓝屏历史、应用崩溃、服务故障、登录审计，可自定义查询
7. 设备与驱动健康：异常设备定位、网卡/蓝牙/音频/显示现状与驱动版本、外接设备识别
8. 命令交付：把需要队员手动执行的修复命令写成 txt 清单，落在工具包 `outbox\` 下，按风险分档并按序号排列，供逐条复制执行

### 未来计划

自制 Web 操作界面正在开发。会话管理与后端接口已建立，页面交互仍在预览阶段；完成验收后接入便携发行版，保留 TUI 通道。

## 目录结构

### 开发版（本仓库）

```
cst-pilot/
|-- pi.cmd                     TUI 入口（发行版同款）
|-- assets/                    品牌资源：logo.png 及其生成脚本
|-- doc/                       产品、设计、工具与测试文档
|-- pack/                      发行版构建脚本
|-- src/
|   `-- web/                    Web 前端源码及 Pen 画布、色表、字体与动态背景
|-- agent/
|   |-- node_modules/          pi 及依赖（不入库）
|   `-- home/
|       |-- extensions/        扩展：branding 品牌页眉、diagnostics 诊断工具
|       |-- skills/            诊断工具的使用说明
|       `-- bin/, npm/, fff/, sessions/, *.json   运行产物与密钥（不入库）
|-- node/                      Node.js（不入库）
|-- pwsh/, wiztree/, lhm/      便携运行时（不入库）
`-- README.md, AGENTS.md, THIRD-PARTY-NOTICES.md, biome.json, .gitignore
```

### 发行版（pack 产出）

```
cst-pilot/
|-- pi.cmd
|-- assets/                    品牌资源（README 插图）
|-- agent/
|   |-- .runtime/              pi 官方二进制与运行资源（隐藏；经 pi.cmd 调用）
|   `-- home/
|       |-- extensions/, packages/, skills/, bin/
|       `-- APPEND_SYSTEM.md, settings.json, models-store.json, open-tui.json
|-- pwsh/, wiztree/, lhm/
|-- doc/                       不含 test/
|-- licenses/
|-- README.md, AGENTS.md, THIRD-PARTY-NOTICES.md, biome.json
`-- VERSION, SHA256SUMS, BUILD-INFO.json
```

发行包不含密钥与运行态；首跑在 pi 内执行 `/login` 填写 key。


## 注意事项

1. 本仓库只含源码与文档，完整运行环境由 `pack/pack.mjs` 构建。
2. 当前实现中，提示词用 `APPEND_SYSTEM.md`，与常见的 `AGENTS.md` 不同，原因见 [doc/Notice.md](doc/Notice.md)
3. 模型URL和API当然是不包括的。如果你是CST的队员且需要相关资源，请联系你们的委员。
4. WizTree 仅个人使用免费、商业使用需授权，见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。


## 使用方式

从 [Releases](https://github.com/SCAU-CST/cst-pilot/releases/latest) 下载最新的发行版（`cst-pilot-<version>.zip`），把整个目录拷到电脑/U盘或者任何地方，然后双击运行：

```
pi.cmd
```

**首次运行**需在 pi 内执行 `/login` 选择 provider 并填写 API key（凭据写入本机 `agent/home/auth.json`）。

开发版仅代码开发。需要自备环境。

## 致谢

名称中的 **pilot** 致敬本项目所基于的 [pi coding agent](https://github.com/earendil-works/pi)。感谢这一伟大的开源项目。

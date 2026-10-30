# CST Pilot

> Computer Service Team · Portable Diagnostics Kit

<p align="center">
  <img src="assets/logo.png" alt="CST Pilot" width="820">
</p>

<p align="center">
  <b>下载</b>：<a href="https://github.com/SCAU-Computer-Serving-Team/cst-pilot/releases/latest">最新发行版</a> · <a href="https://github.com/SCAU-Computer-Serving-Team/cst-pilot/releases">全部版本</a>
</p>

**CST Pilot** 是计算机维护队（Computer Service Team，**CST**）的便携式专用 Agent，为队员提供现场维修时的 AI 技术支持。

特点：

1. **基于开源项目 Pi**。在 [Pi](https://github.com/earendil-works/pi) 的基础上，针对计算机诊断定制。
2. **专门的诊断能力**。配有诊断提示词、技能说明和工具，可以查看磁盘状态、进程占用、设备与驱动等信息。
3. **操作可控**。诊断查询只读，常规修复由队员手动执行。BitLocker 解锁与解密需在 TUI 或 Web 本地输入或确认。
4. **扫描高效**。使用 WizTree 等第三方工具加速磁盘扫描。
5. **即插即用**。发行版可存放在 U 盘，内置所需运行环境，无需在机主电脑上安装 Node、Python 或 npm。

## 功能

1. 扫描磁盘状态与文件占用，分析 C 盘等磁盘的空间使用情况，给出清理建议。
2. 查看进程的 CPU、内存和 GPU 占用，帮助定位资源占用异常。
3. 获取硬件参数，以及可读取的温度、风扇和电压数据。
4. 查看整机负载：内存、CPU 总占用率、页面文件、开机时长。
5. 盘点开机自启：注册表启动项、启动文件夹、自启服务，识别已禁用的项目。
6. 查询 Windows 事件日志：错误与警告、开关机与蓝屏历史、应用崩溃、服务故障、登录记录。
7. 检查设备与驱动：异常设备、网卡、蓝牙、音频、显示设备及外接设备。
8. 把需要手动执行的修复命令整理成 txt 清单，保存在工具包的 `artifacts/<日期>/runbook/` 下，按风险分档，供队员逐条复制执行。
9. 在 TUI 或 Web 查询 BitLocker 状态，使用恢复密码解锁卷，或经本地确认发起完整解密。
10. 在 Web 导出日间／夜间样式的会话 PNG 或离线 HTML，保存在 `artifacts/<日期>/sessions/`；支持指定日期与选取轮次。

支持终端和本机 Web 界面。启动后执行 `/web`，即可在浏览器中聊天、查看历史会话、切换模型和管理授权。后续迭代与发行验证见[待办与议题](doc/TASKS.md)。

具体用法与权限、硬件限制见 [诊断工具规格](doc/diagnostics/SPEC.md) 与 [工具接口](doc/diagnostics/API.md)。

## 目录结构

### 开发版（本仓库）

```text
cst-pilot/
|-- pi.cmd                     启动入口
|-- launcher.ps1               管理员授权与便携运行环境
|-- assets/                    品牌资源
|-- agent/home/
|   |-- extensions/            诊断、Web、认证等扩展
|   `-- skills/                诊断工具的使用说明
|-- src/web/
|   |-- frontend/              Web 前端源码
|   `-- design/                界面设计稿与资源
|-- doc/                       产品、设计、工具与测试文档
`-- pack/                      发行版构建脚本
```

### 发行版

```text
cst-pilot/
|-- pi.cmd
|-- launcher.ps1
|-- agent/
|   |-- .runtime/              Pi 运行时，由 pi.cmd 调用
|   `-- home/                  扩展、技能、配置与预构建 Web 界面
|-- pwsh/, wiztree/, lhm/       便携诊断程序
|-- assets/, doc/, licenses/   品牌资源、文档与第三方许可
|-- README.md, LICENSE, THIRD-PARTY-NOTICES.md
`-- VERSION, SHA256SUMS, BUILD-INFO.json
```

发行包不含密钥和会话记录。使用后，配置、凭据与会话保存在工具包的 `agent/home/` 下。

## 注意事项

1. 支持 Windows 10/11 x64。启动统一请求 Windows 管理员授权；取消授权或没有管理员凭据时不启动。传感器数据能否读取取决于硬件与驱动。
2. 本仓库用于代码开发，不包含完整便携运行环境。开发准备见 [贡献规范](CONTRIBUTING.md)。
3. 发行包默认使用 DeepSeek 官方 `deepseek-flash`，不包含密钥。队员也可连接 CSTOA 团队服务；所需资源请联系队内委员。联网检索默认使用无需密钥的 Exa MCP。
4. 项目遥测默认启用，未登录时使用工具包安装签名上传，会话统计同时发送到 CSTOA 服务器和 Tim 的 `timserver_1`。CSTOA 登录仅提供经验证的上传账号。可在 `agent/home/telemetry.json` 中将 `enabled` 设为 `false`；收集范围见 [信息收集契约](doc/contract.md)。
5. WizTree 仅个人使用免费，商业使用需授权。本项目采用 [MIT 许可证](LICENSE)，第三方条款见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。

其他运行限制见 [运行时规格](doc/runtime/SPEC.md)。

## 使用方式

从 [Releases](https://github.com/SCAU-Computer-Serving-Team/cst-pilot/releases/latest) 下载发行版 ZIP，完整解压到电脑或 U 盘，然后双击 `pi.cmd`。在 Windows 用户账户控制（UAC）窗口中确认授权；普通账户需要输入管理员凭据。

交互界面优先在已安装的 Windows Terminal（`wt`）中打开；未安装或启动请求失败时使用系统控制台，不安装终端软件。

1. 首次运行时执行 `/login`，为默认 DeepSeek 服务填写 API key，或手动选择其他已授权模型。Web 缺少密钥时会提示配置；新会话默认模型在“设置 → 模型偏好”中选择。
2. 描述机主遇到的问题，让 Agent 调用工具协助排查。
3. 想用浏览器操作时，执行 `/web`。原终端会进入待机，请保持窗口打开。
4. Web 使用完毕后，在账号菜单中选择“退出 CST Pilot”。“退出 CSTOA”只退出账号。

## 致谢

名称中的 **pilot** 致敬本项目所基于的 [pi coding agent](https://github.com/earendil-works/pi)。感谢这个开源项目。

[更新记录](CHANGELOG.md) · [产品规格](doc/SPEC.md) · [文档索引](doc/README.md)

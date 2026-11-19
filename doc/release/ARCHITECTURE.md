# 便携发行与构建架构

本页说明共享可变字体同步、LHM 隔离采集器发布与 Rust 在线升级事务管线，并列出随包外部组件的维护约束。发行要求与红线见 [SPEC.md](SPEC.md)。

## 字体构建与分发

桌面端与本机 Web 界面共用同一份官方原版可变字体 `assets/fonts/SourceHanSansCN-VF.otf`（遵循 SIL OFL 1.1 原版再分发规则，保留随包 `assets/fonts/LICENSE.txt`）。准备流程由纯 Node 脚本 `src/web/frontend/scripts/prepare-fonts.mjs` 完成，开发机、CI 与现场运行均无需安装 Python。

| 项目 | 规则 |
|---|---|
| 上游来源 | Adobe Source Han Sans 固定提交 `a4f7cf94edfb9d7ffbdfc4841de276358bd7e0f2` 的 `Variable/OTF/Subset/SourceHanSansCN-VF.otf` 及许可证，逐文件校验 SHA-256 |
| 字符集与字重 | 保留原版全部 30,926 个 Unicode 码位与 `wght 250–900` 连续字重轴，覆盖常用与生僻汉字、简繁字符及中西文符号 |
| 双端共享托管 | 统一落盘至 `assets/fonts/`（ZIP 压缩后约 `8.6 MB`）。GPUI 桌面端直接加载该文件，`cst-server` 将 `/fonts/*` 映射至 `assets/fonts/*` 供本机浏览器读取，`agent/home/extensions/web/static/` 不重复存放字体副本 |
| 导出与回退 | PNG 导出由本机浏览器加载内置可变字体渲染截图；离线 HTML 导出仅内嵌轻量等宽字体 `JetBrains Mono`，正文字体回退本机字体栈。罕见扩展字符与彩色 Emoji 交由系统回退链渲染 |

## Rust LHM 辅助程序发布

`crates/cst-kit/lhm-host/publish.ps1` 将 C# 采集源码发布为 `win-x64` 自包含 .NET 10.0.14 程序，输出可执行文件、运行时与许可证至 `lhm/host/`。中间编译产物与 NuGet 缓存写入仓库外显式指定的 `WorkDirectory`，构建期间关闭 .NET CLI 遥测与开发证书生成。

辅助程序在独立加载上下文中解析上级目录 `lhm/` 中的 `LibreHardwareMonitorLib.dll` 及其依赖，由 Rust 诊断模块挂起启动并绑定 Job Object 控制。

## Rust 升级包准备

`crates/cst-kit/src/update/` 与 `crates/cst-updater/` 分工完成从远端清单检查、流式安全下载到独立进程原子替换的完整闭环：

| 模块 | 职责 |
|---|---|
| `manifest.rs` | 编译期公钥绑定、清单与校验表双重 Ed25519 验签、语义版本与最低升级基线判定 |
| `network.rs` | 复用 SSRF 的 URL 与 DNS 校验，固定解析后的 IP，关闭环境代理，仅允许同源 HTTPS 重定向，清单与归档各享 10 秒请求预算 |
| `verify.rs` | 流式下载并限制体积，解压前校验 ZIP 中央目录上限，逐条拒绝链接与越界路径，复核全树 SHA-256 并执行凭据扫描 |
| `layout.rs` | 划分只读程序区、四份可合并配置与持久用户数据区，调用 Win32 检查磁盘剩余空间（不少于 2 倍解压体积加归档体积） |
| `json.rs` | 有界严格 JSON 解析，拒绝重复字段，防止凭据字段被同名后置键覆盖 |
| `applier.rs` / `io.rs` | 写入 `.state/update/transaction.json` 预写事务，逐文件备份与同卷移动（最多 50 次短暂占用退避），移动前后复核哈希，支持任意边界中断恢复与逆序回滚 |
| `lock.rs` / `process.rs` | 工具包级命名管道升级互斥锁，核对父进程 PID 与创建时间并等待退出（上限 60 秒），驱动 15 秒 Job Object 自检与原界面重启 |
| `handoff.rs` / `bin/cst-updater.rs` | 将已验签更新器复制至 `.state/update/cst-updater-<UUID>.exe`，写入 `.state/update/requests/<UUID>.json`，`cst-updater.exe` 接收 `--root <目录> --request <id>` 或 `--recover`，库层导出 `handoff::health_check` 供主程序接入 |
| `service.rs` / `activity.rs` | 同 home 共享更新状态与后台首检，管理 `ActivityGate` 写入门禁、`/update` 命令及 5 秒遥测定稿协调 |

### 更新交接与恢复

`Handoff::prepare` 二次复核签名、文件哈希与父进程身份，复制独立更新器并写出交接请求。未启动即丢弃句柄时自动清理暂存目录。调用 `launch` 后由宿主退出当前进程（阶段 9 升级为桌面与后台双进程协同退出）。`cst-updater.exe` 仅信任自身编译期公钥，不从命令行接收公钥或任意执行路径。

替换阶段将旧文件移入 `.state/update/backup/<事务UUID>/files/`，合并后的配置候选放入 `prepared/`，最后发布根目录 `VERSION`。新版通过 `--health-check` 后先写入提交标记，再清理备份与暂存并拉起原界面。自检失败或超时立即使用独立取消令牌逆序恢复旧版，并在 `.state/update/result.json` 记录结果类别供下次启动提示。

### 宿主更新服务

`UpdateService` 在同一工具包 home 下共享状态，`ActivityGate` 在输入持久化、模型投递、工具运行、清单写入与配置修改入口统一发放租约。发起升级交接时先关闭门禁并确认全部已加载会话空闲且无待处理队列。交接失败时重新开放门禁，允许用户稍后重试。

### 更新发现与发布源

当前 `service.rs` 默认清单地址为 `<oaHost>/api/agent/releases/latest`（官方地址 `https://www.cstoa.top`），阶段 9 统一要求编译期同时注入 `CST_UPDATE_MANIFEST_URL` 与 `CST_UPDATE_PUBLIC_KEY`。客户端读取根目录 `VERSION` 中的 `cst-pilot <版本>` 行与签名清单比对。缺少编译期升级参数时状态固定为不可用且不发起网络请求。

### 签名清单协议

远端接口返回 `{ payload, signature }`，`payload` 为完整原始 JSON 字符串，`signature` 为对 UTF-8 `cst-pilot-release-manifest-v1\n` 拼接 `payload` 字节计算的 Base64 Ed25519 签名：

| `payload` 字段 | 约束 |
|---|---|
| `schemaVersion` / `platform` | 固定为 `1` / `windows-x86_64` |
| `version` / `minUpgradableVersion` | 语义版本，安装前重读本机 `VERSION` 二次核验 |
| `builtAt` / `releaseNotes` | RFC 3339 构建时间，更新说明上限 `16 KiB` |
| `archive` | `{ url, size, unpackedSize, sha256 }`。归档上限 `2 GiB`，解压上限 `8 GiB` / `100,000` 个文件，ZIP 中央目录上限 `64 MiB` |
| `sha256Sums` / `sha256Signature` | `64位小写哈希 + 两个空格 + 相对路径` 全树清单，配 `cst-pilot-release-sums-v1\n` 前缀独立签名 |

## 随包外部组件与过渡期装配

| 组件或脚本 | 用途与维护约束 |
|---|---|
| `wiztree/` | NTFS MFT 高速空间扫描与 CSV 导出。个人使用免费，商业使用需授权。打包排除 `WizTree3.ini`、`.bad` 与 `tmp/` |
| `lhm/` 与 `lhm/host/` | `LibreHardwareMonitorLib.dll`（MPL-2.0）及 .NET 10 自包含隔离采集宿主。遵循零安装约束不附带 PawnIO 驱动，保留 `DiskInfoToolkit.dll` 等依赖便于后续扩展 |
| `pack/pack.mjs`（阶段 11 前过渡） | 校验锁文件、构建 Web 前端并同步共享可变字体、执行 `checkReleaseTree` / `checkReleaseContent` 红线扫描及 `_smoke` 隔离冒烟，最后生成 ZIP 并复核 `SHA256SUMS` |

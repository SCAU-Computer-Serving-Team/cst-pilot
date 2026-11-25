# Windows 10 / 11 与中国大陆特制版兼容性测试手册

本文列出 CST Pilot 在 Windows 10/11 x64 及常见定制镜像上的兼容性风险、Win11 本地验证方法和发布前真机验收项。产品支持范围见[产品规格](../../SPEC.md#全局约束)；精简镜像和 WinPE 需单独验证。

## 1. 目标系统矩阵与风险分级

桌面端依赖 Win32、Direct3D 11、DirectWrite、DWM、WMI、Windows CryptoAPI 与便携文件自定位。不同系统版本和特制版对上述子系统的裁剪与策略存在显著差异。

| 系统版本 / 特制形态 | 代表 Build 号 | 典型来源 | 关键系统特征 | 风险等级 |
|---|---|---|---|---|
| Windows 11 24H2 / 25H2（专业/家庭/家庭中文版） | `26100` / `26200` | 较新的个人电脑 | 视设备与策略启用内存完整性（HVCI）；检查驱动、SmartScreen 和杀软拦截 | 高 |
| Windows 11 22H2 / 23H2（专业/家庭/家庭中文版） | `22621` / `22631` | 主流现役轻薄本与游戏本 | 支持 `DWMWA_SYSTEMBACKDROP_TYPE`（`>= 22621`）与 `DWMWA_WINDOW_CORNER_PREFERENCE`（`>= 22000`）；家庭中文版无完整 BitLocker 管理工具 | 中 |
| Windows 11 21H2 | `22000` | 早期 Win11 升级设备 | 支持窗口圆角与暗色标题栏，不支持 `DWMWA_SYSTEMBACKDROP_TYPE` | 低 |
| Windows 10 21H2 / 22H2（含 LTSC 2021） | `19044` / `19045` | 老旧笔记本、实验室与机房 | 无 Win11 圆角和系统背景材质；LTSC 上应单独核查所需组件 | 中 |
| Windows 10 1809（含 LTSC 2019） | `17763` | 工控机、教学机房、老旧办公机 | 检查暗色标题栏的属性 `19` 回退、中文字体和旧显卡驱动 | 高 |
| Windows 10 / 11 家庭中文版（Home China） | 随功能更新变化 | 中国大陆品牌 OEM 预装个人机 | 覆盖中文路径、非 UTF-8 控制台，以及 BitLocker 接口与设备加密的实际可用性 | 高 |
| 神州网信政府版（CMGE） | 按实机记录版本与 Build | 政企、高校行政与国产化采购设备 | 核查应用与脚本执行、WMI、证书和网络策略；以实机配置为准 | 高 |
| 校园 / 机房精简版与 WinPE | 各类定制镜像 | 维修 U 盘、第三方精简系统 | 可能缺失系统字体（如 `Segoe UI` / `Microsoft YaHei UI`）、缺少 WMI 服务或无硬件 WDDM 驱动（回退 WARP）、缺失 PowerShell 7、禁用透明效果与动效 | 高 |

## 2. 源码依赖与八大兼容性风险面

下表按本仓库真实调用路径列出关键风险、当前代码兜底机制与重点核查项。

| 风险面 | 仓库真实依赖位置 | 特制版 / 低版本表现 | 期望兜底行为 |
|---|---|---|---|
| **1. DWM 窗口属性** | `vendor/gpui-pre-windows/src/{window,util}.rs` | Win10 无 Win11 圆角与系统背景材质；部分旧版 Win10 的暗色属性号为 `19` | 按版本设置圆角与系统背景材质；暗色属性 `20` 返回 `E_INVALIDARG` 时尝试 `19`，两者均不支持时保留默认外观 |
| **2. Direct3D 11 与着色器** | `vendor/gpui-pre-windows/src/directx_devices.rs`、`crates/cst-desktop/src/background/d3d11_surface.rs` | 老驱动可能不支持特性级 `11_0`；没有可用硬件适配器时可能使用 WARP 软件光栅 | D3D11 可用时，低于 `11_0` 或软件光栅下用静态背景和实底浮层；系统图形组件缺失需单独实测 |
| **3. 字体与 DirectWrite** | `crates/cst-desktop/src/theme/typography.rs`、`vendor/gpui-pre-windows/src/direct_write.rs` | 精简镜像可能裁剪系统字体，解压不完整可能缺少随包字体 | 校验随包思源黑体和 JetBrains Mono 的文件头、字体表目录并注册。失败时提示并选用 `Microsoft YaHei UI` / `Consolas`；仍需检查系统回退字体是否可用 |
| **4. BitLocker WMI 与权限** | `crates/cst-kit/src/bitlocker/wmi_encryptable.rs`（`root\cimv2\Security\MicrosoftVolumeEncryption` / `Win32_EncryptableVolume`） | 家庭中文版可能无此命名空间或只读设备加密；标准用户权限不足；政府版策略受限 | 按明确中文提示返回受限/不支持原因与 `collectionErrors`，不得崩溃、不得把未枚举卷误报为「未加密」 |
| **5. 硬件传感器驱动与国产杀软** | `crates/cst-kit/lhm-host/`（硬件监控）、`wiztree/`、未签名或新发行的 `cst-{pilot,server,updater}.exe` | Win11 24H2 HVCI 内核隔离拦截驱动；360 / 火绒 / 腾讯电脑管家 / Defender SmartScreen 拦截或拖慢子进程建链 | 传感器采集与 `wiztree` 失败时降级返回可读原因（`wiztree` 自动降级 `node-walk`）；`cst-server` 启动超时与端口被拦截时给出清晰排障提示 |
| **6. TLS 根证书与校园网/代理** | `reqwest` + `rustls-platform-verifier`（走 Windows 系统根证书库）、`crates/cst-kit/src/auth/oa.rs` | 政府版/离线 LTSC 根证书长期未更新导致 HTTPS 握手失败；校园网认证劫持返回 HTML 或 3xx 重定向；TUN 代理或系统代理影响本地回环 | 本地 `cst-server` 回环通信禁止走系统代理；OA 请求严格拒绝 3xx 重定向与非预期响应体，并给出明确网络/代理提示 |
| **7. 中文路径与控制台** | `crates/cst-kit/src/kit.rs`、`pi-desktop-rust.cmd` | 中文用户名与路径、非 UTF-8 控制台、仅有 Windows PowerShell 5.1 | 核查便携路径定位、日志和文件写入；原生二进制单独验证。本文的开发测试脚本需要 PowerShell 7 |
| **8. 软件内升级与单实例锁** | `crates/cst-updater/`、`crates/cst-kit/src/update/`、`crates/cst-server/src/locks.rs` | 杀软扫描锁定新替换文件；只读目录（如 `Program Files` 无写权限或只读 U 盘）；高/低权限混开双实例 | 权限不匹配与跨会话冲突安全拦截；文件替换保留预写事务与 15 秒健康检查回滚，禁止半成品破坏原目录 |

## 3. 无模拟平台时的本地等效验证方案

可在 Win11 上核查导入表，验证 WARP、中文路径与隔离环境下的权限及代理行为。旧版系统的加载、界面和组策略仍需真机验证。

### 3.1 静态 PE 导入表与运行时依赖审计

检查三个 Release 二进制的普通导入和延迟导入，核对最低目标系统上的 DLL 与入口点。

```powershell
# 在 Developer PowerShell 或已安装 LLVM/VS Build Tools 环境中检查导入 DLL 列表
foreach ($exe in @('target\release\cst-pilot.exe', 'target\release\cst-server.exe', 'target\release\cst-updater.exe')) {
    Write-Host "=== $exe ==="
    dumpbin /imports $exe
    if ($LASTEXITCODE -ne 0) { throw "无法读取导入表：$exe" }
}
```

核验红线：

1. 确认目标系统提供所需系统 DLL 和入口点。对 `VCRUNTIME140.dll` 等独立运行库，核查发行包是否已提供合法分发的运行库，或在构建时改用静态链接。
2. 高版本才有的入口点需动态解析并处理不可用情况，调用前的版本判断无法补救静态链接的缺失入口点。`DWMWA_SYSTEMBACKDROP_TYPE` 是属性编号，检查的是调用时的版本门控。

### 3.2 本地降级开关与极端环境注入

在主力机上无需安装旧系统即可直接验证的核心兼容路径：

| 验证目标 | 本地等效模拟操作 | 预期通过标准 |
|---|---|---|
| WARP 渲染降级 | 用 `test-support` 构建并运行 [UX 脚本的 `H-WARP` 档](ux-e2e.md#22-开发机资源限制与补测档位) | 检查 `supportsSm50 = false`、`frostedDrawCount = 0`，并查看主页、登录页和浮层的显示。WinPE 的系统组件仍需另外验收 |
| 系统关闭透明效果 / 精简版无动效 | Windows 设置中关闭「透明效果」和「动画效果」（或分别单独关闭） | 毛玻璃浮层自动切为实底 `$bg-elevated`，动效位移归零，关闭/展开逻辑无卡死 |
| 中文用户名 + 空格 + 深层路径 | 将发布包复制到 `E:\tmp\<YYYY-MM-DD>\测试 用户 (中文)\cst pilot\` 下直接启动 | 便携目录自定位成功，自带字体正常加载，日志、配置、会话、导出 HTML/PNG 全流程无乱码或找不到路径 |
| 多 DPI 缩放与窗口羽化 | 分别在 `100%`、`125%`、`150%`、`175%`、`200%` 打开登录页、主页、设置页与模型选择器 | 登录页两侧交界无可见竖线。每档按截图重新定位扫描区域；文字无裁切，浮层不越出屏幕边缘 |
| 标准用户（非管理员）受限权限 | 仅对隔离实例设置 `CST_ALLOW_UNELEVATED=1`，调用 `disk(health)`、BitLocker 状态查询和诊断导出 | 需提权的能力返回可读提示，其余只读诊断正常返回。正式包的 UAC 流程按真机清单单独核查 |
| 系统代理与 TUN 代理并存 | 开启本机 TUN / HTTP 代理后运行软件端 + 网页端双通道切换与 CSTOA 扫码 | 本机 `cst-server` 回环连接零超时，SSE 聚合流不断连，扫码登录与账号资料查询正常 |
| 离线与慢盘启动 | 设置 `$env:PI_OFFLINE = '1'`，并运行 `H-SlowIO` 延迟注入 | 不阻塞主界面呈现，不出现未捕获网络异常崩溃 |

## 4. 真机众测最小验收清单（发布前使用）

发布前按以下清单记录通过、失败或未测。遇到策略或硬件限制时，附截图或日志，并写明受限项。

### 4.1 环境信息（测试前采集）

```powershell
$os = Get-CimInstance Win32_OperatingSystem
$cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
$gpu = (Get-CimInstance Win32_VideoController | ForEach-Object { "$($_.Name) ($($_.DriverVersion))" }) -join '; '
$av = (Get-CimInstance -Namespace root\SecurityCenter2 -ClassName AntiVirusProduct -ErrorAction SilentlyContinue | ForEach-Object { $_.displayName }) -join ', '
[PSCustomObject]@{
    OS = "$($os.Caption) | Build $($os.BuildNumber) | $($os.OSArchitecture)"
    Locale = (Get-WinSystemLocale).Name
    CPU = $cpu.Name
    RAM_GiB = [math]::Round($os.TotalVisibleMemorySize / 1MB, 1)
    GPU = $gpu
    AntiVirus = if ($av) { $av } else { 'Unknown/Server' }
} | Format-List
```

### 4.2 最小验收项（10 项，勾选通过/失败并附截图或日志）

| 编号 | 验收项 | 覆盖风险 | 合格判据 |
|---|---|---|---|
| `C01` | 解压到含中文与空格路径并从发行入口启动 | 中文路径、便携定位、杀软首启拦截 | 确认 UAC 后 5 秒内出现主界面。记录任何 SmartScreen 或杀软提示；无缺 DLL 弹窗或静默退出 |
| `C02` | 中文字体与等宽代码渲染 | 随包字体加载、精简版缺字 | 中文标题、正文与代码块显示为思源黑体与等宽字体，无豆腐块或行高裁切 |
| `C03` | 登录页浅/深色切换与扫码刷新 | DWM、着色器/WARP 降级、羽化缝隙、OA 证书 | 背景平滑无 1px 竖线，二维码清晰可扫，点「刷新」立即更新，无 TLS 证书报错 |
| `C04` | CSTOA 登录后查看账号页与模型选择 | 账号资料接口、思考档位、缓存 | 校验账号和额度，选可用的 DeepSeek 模型完成一次对话。回传截图前遮盖个人信息 |
| `C05` | 打开设置 → 模型偏好并滚动、搜索 | 虚拟列表性能、已登录服务商过滤 | 仅展示已就绪服务商的模型；滚动可继续显示后续条目，筛选结果与关键词匹配 |
| `C06` | 中文输入法（微软拼音 / 搜狗 / 微信输入法）组合输入 | IME 组合态回车拦截、焦点管理 | 拼音候选态按 `Enter` 只上屏不误发消息，按 `Esc` 只取消候选不关弹窗 |
| `C07` | 执行只读诊断（磁盘、网络、事件日志、系统信息） | WMI、家庭版/政府版受限接口、HVCI | 正常返回结构化结果；遇权限或策略受限时给出清晰中文原因而不崩溃 |
| `C08` | 在家庭中文版或受限账号下触发 BitLocker 状态查询 | `Win32_EncryptableVolume` 差异 | 正确区分支持/不支持/权限不足，不把未枚举卷误报为未加密 |
| `C09` | 窗口缩至 `1024×640` 并切至网页端再切回桌面端 | 多 DPI 几何、单实例与双通道门禁 | 窄窗自动收起侧栏无重叠，切回桌面端自动同步状态与账号信息 |
| `C10` | 关闭主窗口并检查任务管理器 | 双进程优雅退出 | 5 秒内 `cst-pilot.exe` 与 `cst-server.exe` 全部退出，无后台残留进程 |

## 5. 本地等效实测记录

2026-11-22 在 Windows 11 x64（`10.0.26200`，应用进程未提权，`125%` 缩放）运行隔离 Release 实例。下表的动态结果来自 `F:\tmp\2026-11-22\compat-run\compat-report.json`；PE 导入为单独的静态检查。

| 检查项 | 实测结果 | 发现与处置 |
|---|---|---|
| PE 导入表（3 个 `dist` 发行二进制） | 普通及延迟导入中无 `WebView2`、`icuuc.dll`、`VCRUNTIME140.dll` 或 `MSVCP*.dll`，仅依赖 Windows 内置系统 DLL | `xtask dist --profile dist` 在 Windows 下默认附加 `-C target-feature=+crt-static` 静态链接 CRT；`destination_list.rs` 使用 Rust 在缓冲区内找 NUL，去掉了 `u_strlen` 依赖 |
| 旧版 Win10 DWM 暗色边框 | 已实现回退，待真机验证 | `configure_dwm_dark_mode` 仅在属性 `20` 返回 `E_INVALIDARG` 时尝试属性 `19` |
| 中文、空格与括号路径（`测试 用户 (中文)\cst pilot`） | 启动、打开登录页和双进程退出均通过 | 探针首帧 `409.15 ms`；脚本在 `869.7 ms` 确认可取到窗口与首帧探针；点击关闭后 `2123.2 ms` 内两个进程退出 |
| 无效代理环境变量（`HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY` 均指向 `127.0.0.1:9`）及 `PI_OFFLINE=1` | 本机回环可用 | 后端健康检查、`/api/models` 请求及桌面端启动成功；未测真实代理下的 CSTOA 登录 |
| 常规设置与模型偏好 | 报告通过 | 写入 `defaultView: "tui"` 后，常规设置未出现 `settings-view-tui`。目录中就绪服务商仅 `deepseek`，目标 `ant-ling` 条目未出现；`DeepSeek V4.1 Flash` 返回 `off`、`low`、`high`、`max` |
| 登录页竖线扫描（深色 / 浅色 / WARP 深色，`125%` 缩放） | 3 项通过 | 截图区 `[1060, 150, 40, 700]` 内，报告记录的峰值依次为 `0.055`、`0.215`、`0.927`。用当前脚本复算，取单列与两侧同向亮度差的较小值，峰值依次为 `0.041`、`0.009`、`0.572`，均低于 `3.0`。WARP 下 `supportsSm50 = false`、`frostedDrawCount = 0` |
| 管理员提权 `H-GpuClamp` 与 `200%` DPI 登录页竖线扫描 | 通过（核心实测 `172–180 MHz`） | `2560×1600` 登录页 ROI `[1696, 240, 64, 1120]` 内深色峰值 `0.284`、浅色峰值 `0.345`（均 `< 3.0`）。显存频率在活动显示 `P0` 下保持 `14001 MHz`，完整分项指标见 [UX E2E 第 6.3 节](ux-e2e.md#63-h-gpuclamp-提权锁频与-200-dpi-补测release) |
| 管理员原生模式双进程启停与 BitLocker WMI 对照 | 通过 | 管理员上下文移除 `CST_ALLOW_UNELEVATED` 后正常启动（首帧 `368.26 ms`）并在 `2195.1 ms` 内双进程优雅退出；由提权脚本直接调用 CIM 核验 `Win32_EncryptableVolume` 枚举本机 5 个卷（`C:`–`G:`），作为管理员 WMI 基线对照（未走 `cst-kit` 工具链路） |

以上结果仅覆盖表内场景。旧系统（Win10 1809 / 神州网信政府版 / 家庭中文版）、定制镜像、字体回退、真实 CSTOA 登录和杀软策略仍按第 4 节真机清单验收。

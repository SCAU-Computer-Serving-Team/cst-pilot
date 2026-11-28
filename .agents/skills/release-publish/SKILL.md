---
name: release-publish
description: Sign, verify, upload, and activate a CST-Pilot Windows x64 portable release package (.zip, dist bundle, or unpacked kit directory) to official stable or beta update channels via pre-compiled xtask and OpenSSH stdio forwarding. Use when the user provides a release package to publish, asks to cut and publish a new release, or asks to verify or roll back an online update channel.
---

# 发行包签名与上线发布（Release Publish）

本技能定义从本地发行产物（`.zip`、`xtask dist` 产物目录或解压后的 `cst-pilot/` 目录）到官方更新源上线激活的标准流程。全程先在无凭据环境下预编译 `xtask.exe`，再由预编译程序完成离线验包与双重 Ed25519 签名，并经系统 OpenSSH `-W` 标准流直连远端回环接口完成上传与公网复核。

## 前置核查

1. **设置编译进程亲和性**：执行任何 `cargo` 命令前，先设置当前 PowerShell 进程的逻辑核心上限，预留 2 个核心给系统：
   ```powershell
   $keep = [Math]::Min(62, [Math]::Max(1, [Environment]::ProcessorCount - 2))
   (Get-Process -Id $PID).ProcessorAffinity = [IntPtr](([int64]1 -shl $keep) - 1)
   ```
2. **在无凭据环境中预编译 `xtask`**：先移除当前进程的发布凭据变量，编译独立的 `target/release/xtask.exe`，避免外层 Cargo 及构建脚本继承私钥路径或管理令牌：
   ```powershell
   Remove-Item Env:CST_RELEASE_PRIVATE_KEY_FILE, Env:CST_RELEASE_ADMIN_TOKEN -ErrorAction SilentlyContinue
   cargo build --release -p xtask --locked
   ```
3. **核验线上通道基线**：请求目标通道公开清单（正式通道 `https://www.cstoa.top/api/agent/releases/latest`，内测通道 `https://www.cstoa.top/api/agent/releases/beta`）。首次发布返回 `HTTP 404`（`channel_empty`）；已有线上版本时确认待发版本号高于当前激活版本，并与用户确认 `--min-upgradable` 基线、更新说明及待发包 SHA-256。

## 发布路径选择

根据用户提供的输入形态选择对应参数。

| 输入形态 | 适用命令与参数 | 安全核验机制 |
|---|---|---|
| 独立发行包（现成 `.zip`，或同级含规范 `.zip` 的解压目录） | `xtask.exe release --package <ZIP路径> --expected-archive-sha256 <64位摘要>` | 解包前先在内存中核对带外核准的归档 SHA-256，再直接从内存归档以零子进程静态核验 PE32+ 节表与入口点、`.rdata` 唯一 `CST_UPDATE_ROOT_V1` 通道与公钥标记、`BUILD-INFO.json`、预置配置与全套许可证、私钥防夹带扫描及 `VerifiedPackage` 闭环自验 |
| `xtask dist` 产物目录（含 `.zip` 与 `.attestation.json`） | `xtask.exe release --package <目录或ZIP> --expected-attestation-sha256 <摘要> --expected-commit <40位提交>` | 解包前先带外核对构建证明 SHA-256、干净 Git 提交哈希、目标通道与归档摘要，再执行双重签名与闭环自验 |
| 从干净 `main` 现编并发布 | 先在无凭据环境执行 `xtask.exe dist --channel <stable\|beta>`，再执行带证明的 `xtask.exe release` | 锁定构建前后同一 40 位 `HEAD`，提取本次 Cargo 编译产出的可执行路径，完成 PE 头、`.rdata` 静态标记、双进程 `--key-fingerprint` 与隔离冒烟核验 |

## 标准执行步骤

1. **按需构建干净发行包**（用户已直接提供发行包时跳过）：保持当前进程不含私钥与管理令牌变量，在仓库外临时目录（如 `E:/tmp/<YYYY-MM-DD>/dist`）执行打包（正式无密钥构建机额外附加 `--strict-keyless-env`）：
   ```powershell
   .\target\release\xtask.exe dist --out E:/tmp/<YYYY-MM-DD>/dist --version vX.Y.Z --profile dist --channel stable
   ```
2. **执行离线预演**：仅向预编译 `xtask.exe` 提供私钥路径（离线预演无需 `CST_RELEASE_ADMIN_TOKEN`），先附加 `--dry-run` 跑通摘要绑定、信任根核验、双重签名与内存解包自验：
   ```powershell
   $privKey = [Environment]::GetEnvironmentVariable('CST_RELEASE_PRIVATE_KEY_FILE', 'User')

   # 场景 A：用户直接提供独立发行 ZIP（先将本地摘要与用户带外核准值严格比对）
   $approvedSha = '<用户带外核准的64位小写SHA256>'
   $localSha = (Get-FileHash <ZIP路径> -Algorithm SHA256).Hash.ToLowerInvariant()
   if ($localSha -ne $approvedSha) { throw "本地归档摘要 ($localSha) 与用户核准值 ($approvedSha) 不一致，停止发布。" }
   .\target\release\xtask.exe release --package <ZIP路径> --min-upgradable vX.Y.Z --channel stable --expected-archive-sha256 $approvedSha --private-key-file $privKey --notes-file <说明文件.md> --dry-run

   # 场景 B：带构建证明的 dist 产物（记录构建输出的证明摘要与 40 位提交哈希）
   .\target\release\xtask.exe release --package <dist目录或ZIP> --min-upgradable vX.Y.Z --channel stable --expected-attestation-sha256 $attSha --expected-commit $commitSha --private-key-file $privKey --notes-file <说明文件.md> --dry-run
   ```
3. **经 OpenSSH 标准流正式上线**：仅在最终上传步骤读取用户级 `CST_RELEASE_ADMIN_TOKEN` 注入子进程环境，复用预演阶段同一组带外核准摘要参数，移除 `--dry-run` 并显式传入 `--ssh-host <别名>`。`xtask.exe release` 不绑定任何本机 TCP 端口，直接通过系统 OpenSSH `-W` 匿名管道流式上传内存中已自验的签名清单与归档副本，激活后向公网核验清单签名与归档 `HEAD` 响应：
   ```powershell
   $env:CST_RELEASE_ADMIN_TOKEN = [Environment]::GetEnvironmentVariable('CST_RELEASE_ADMIN_TOKEN', 'User')
   try {
       # 场景 A（独立发行 ZIP）：
       .\target\release\xtask.exe release --package <ZIP路径> --min-upgradable vX.Y.Z --channel stable --expected-archive-sha256 $approvedSha --private-key-file $privKey --notes-file <说明文件.md> --ssh-host cstoa

       # 场景 B（带构建证明的 dist 产物）：
       # .\target\release\xtask.exe release --package <dist目录或ZIP> --min-upgradable vX.Y.Z --channel stable --expected-attestation-sha256 $attSha --expected-commit $commitSha --private-key-file $privKey --notes-file <说明文件.md> --ssh-host cstoa
   } finally {
       Remove-Item Env:CST_RELEASE_ADMIN_TOKEN -ErrorAction SilentlyContinue
   }
   ```
   需预先上传但暂不切换公开通道指针时，附加 `--no-activate`。
4. **紧急回滚通道指针**：线上版本需回退时，通过 SSH 调用服务端接收器子命令原子切回已入库的历史版本，随后重新请求公开清单确认生效：
   ```powershell
   ssh <ssh-host> "<telemetry-receiver> release activate --channel stable --version <历史版本号>"
   ```

## 安全红线

- 严禁在终端输出、文档或提交记录中泄露私钥内容、`CST_RELEASE_ADMIN_TOKEN` 明文，以及真实服务器 IP 或服务端内网路径。
- 严禁在注入 `CST_RELEASE_PRIVATE_KEY_FILE` 或 `CST_RELEASE_ADMIN_TOKEN` 的环境中直接执行 `cargo build` 或 `cargo run`；统一先在无凭据环境编译 `xtask.exe`，再直接调用预编译二进制。
- 严禁在持有发布私钥的账户下运行外部待签包内的任何 `.exe`，也严禁对未经带外摘要或构建证明核准的归档执行签名。
- 严禁将 `.zip`、`*.attestation.json`、`manifest.signed.json` 及密钥令牌文件提交至 Git 仓库。所有临时过程产物均存放在 `E:/tmp/<YYYY-MM-DD>/` 或 `F:/tmp/<YYYY-MM-DD>/`。

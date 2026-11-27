---
name: release-publish
description: Sign, verify, upload, and activate a CST-Pilot Windows x64 portable release package (.zip, dist bundle, or unpacked kit directory) to official stable or beta update channels via xtask and scoped SSH loopback tunnel. Use when the user provides a release package to publish, asks to cut and publish a new release, or asks to verify or roll back an online update channel.
---

# 发行包签名与上线发布（Release Publish）

本技能定义从本地发行产物（`.zip`、`xtask dist` 产物目录或解压后的 `cst-pilot/` 目录）到官方更新源上线激活的标准流程。全程由 `crates/xtask` 执行离线验包与双重 Ed25519 签名，再经临时回环 SSH 隧道上传并完成公网复核。

## 前置核查

1. **核验本机凭据变量**：确认当前进程或 Windows 用户级环境变量已配置 `CST_RELEASE_PRIVATE_KEY_FILE`（离线私钥文件路径）与 `CST_RELEASE_ADMIN_TOKEN`（管理端令牌）。长生命周期宿主进程未继承最新用户级变量时，在 PowerShell 内存中通过 `[Environment]::GetEnvironmentVariable('<变量名>', 'User')` 注入当前进程，严禁打印变量内容。
2. **核验线上通道基线**：请求目标通道公开清单（正式通道 `https://www.cstoa.top/api/agent/releases/latest`，内测通道 `https://www.cstoa.top/api/agent/releases/beta`）。首次发布返回 `HTTP 404`（`channel_empty`）；已有线上版本时确认待发版本号高于当前激活版本，并与用户确认 `--min-upgradable` 基线及更新说明。
3. **设置编译进程亲和性**：执行任何 `cargo` 命令前，先设置当前 PowerShell 进程的逻辑核心上限，预留 2 个核心给系统：
   ```powershell
   $keep = [Math]::Min(62, [Math]::Max(1, [Environment]::ProcessorCount - 2))
   (Get-Process -Id $PID).ProcessorAffinity = [IntPtr](([int64]1 -shl $keep) - 1)
   ```

## 发布路径选择

根据用户提供的输入形态选择对应参数。

| 输入形态 | 适用命令与参数 | 安全核验机制 |
|---|---|---|
| 独立发行包（单个 `.zip` 或解压目录，无构建证明） | `xtask release --package <路径> --unattested-package` | 纯 Rust 零子进程静态核验 PE64 GUI 头、`CST_UPDATE_ROOT_V1` 通道与公钥标记、`BUILD-INFO.json`、干净树红线及 `VerifiedPackage` 闭环自验 |
| `xtask dist` 产物目录（含 `.zip` 与 `.attestation.json`） | `xtask release --package <目录或ZIP> --expected-attestation-sha256 <摘要> --expected-commit <40位提交>` | 带外核对构建证明 SHA-256 与干净 Git 提交哈希，校验目标通道一致后再执行双重签名与闭环自验 |
| 从干净 `main` 现编并发布 | 先执行 `xtask dist --channel <stable\|beta>`，再执行带证明的 `xtask release` | 自动剥离构建环境密钥变量，锁定构建前后同一 40 位 `HEAD`，执行隔离冒烟与双进程 `--key-fingerprint` 自检 |

## 标准执行步骤

1. **按需构建干净发行包**（用户已直接提供发行包时跳过）：在仓库外临时目录（如 `E:/tmp/<YYYY-MM-DD>/dist`）执行打包：
   ```powershell
   cargo run -p xtask --locked -- dist --out E:/tmp/<YYYY-MM-DD>/dist --version vX.Y.Z --profile dist --channel stable
   ```
2. **执行离线预演**：先附加 `--dry-run` 跑通静态红线与信任根核验，完成双重签名和归档解包自验后再发起上传：
   ```powershell
   # 场景 A：用户直接提供单个 ZIP 或解压目录
   cargo run -p xtask --locked -- release --package <包路径> --min-upgradable vX.Y.Z --channel stable --unattested-package --notes-file <说明文件.md> --dry-run

   # 场景 B：带构建证明的 dist 产物
   cargo run -p xtask --locked -- release --package <dist目录或ZIP> --min-upgradable vX.Y.Z --channel stable --expected-attestation-sha256 <64位摘要> --expected-commit <40位提交> --notes-file <说明文件.md> --dry-run
   ```
3. **经临时隧道正式上线**：移除 `--dry-run` 并传入 SSH 主机别名（如 `--ssh-host cstoa`）。`xtask release` 自动建立仅绑定 `127.0.0.1` 随机端口的 OpenSSH 隧道，流式上传签名清单与归档，激活通道后销毁隧道，并向公网核验清单签名与归档 `HEAD` 响应：
   ```powershell
   cargo run -p xtask --locked -- release --package <包路径> --min-upgradable vX.Y.Z --channel stable --unattested-package --notes-file <说明文件.md> --ssh-host cstoa
   ```
   需预先上传但暂不切换公开通道指针时，附加 `--no-activate`。
4. **紧急回滚通道指针**：线上版本需回退时，通过 SSH 调用服务端接收器子命令原子切回已入库的历史版本，随后重新请求公开清单确认生效：
   ```powershell
   ssh <ssh-host> "<telemetry-receiver> release activate --channel stable --version <历史版本号>"
   ```

## 安全红线

- 严禁在终端输出、文档或提交记录中泄露私钥内容、`CST_RELEASE_ADMIN_TOKEN` 明文，以及真实服务器 IP 或服务端内网路径。
- 严禁在持有发布私钥的账户下直接运行外部待签包内的任何 `.exe`。独立发行包统一通过 `--unattested-package` 执行纯静态字节校验。
- 严禁将 `.zip`、`*.attestation.json`、`manifest.signed.json` 及密钥令牌文件提交至 Git 仓库。所有临时过程产物均存放在 `E:/tmp/<YYYY-MM-DD>/` 或 `F:/tmp/<YYYY-MM-DD>/`。

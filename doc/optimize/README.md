# 优化记录

本目录按专题记录项目各次性能与体验优化的背景、技术决策、实测数据与结论。每次优化对应一篇独立文档，方案总览与分阶段进度见 [9.3.optimization.md](../rust/plan/9.3.optimization.md) 和 [TASKS.md](../TASKS.md)。

## 专题清单

| 文档 | 优化对象 | 核心改造 | 决策依据 | 日期 |
|---|---|---|---|---|
| [storage-ls-disk.md](storage-ls-disk.md) | `ls` 与 `disk` 存储诊断 | 纯 Rust `ntfs-ng` `$MFT`、无条目上限并行 Win32 遍历与原生存储 IOCTL | [ADR-014](../ADR.md#adr-014移除-wiztree存储诊断改用纯-rust-mft-与-win32-ioctl) | `2026-11-24` |
| [file-search-fff.md](file-search-fff.md) | `fffind` 与 `ffgrep` 文件检索 | 独立入参契约、`fffind` 全电脑 `$MFT + USN` 双模索引与 `ffgrep` 单文件流式扫描 | [ADR-018](../ADR.md#adr-018文件检索采用单次基线单句柄核验与无状态并行多编码引擎) / [ADR-025](../ADR.md#adr-025fffind-升级全电脑双模检索ffgrep-拆分独立契约并支持单文件流式扫描) | `2026-11-26` |
| [system-diagnostics-native.md](system-diagnostics-native.md) | `sys`、`startup`、`driver`、`eventlog` | `NtQuerySystemInformation` 单次快照、Win32/SCM/SetupAPI 替代 WMI、双显卡核显 LHM 与事件值渲染 | [ADR-023](../ADR.md#adr-023系统诊断改用原生-win32nt-接口与结构化事件渲染) | `2026-11-26` |
| [tools-contract-web-runbook.md](tools-contract-web-runbook.md) | 全量工具契约、联网检索、`runbook` | 统一 `elapsedMs` 与 `read` JSON、Exa/DeepSeek 三工具联网检索、单工具 `runbook` 与一键导出 | [ADR-020](../ADR.md#adr-020全量工具注入-elapsedmsread-改-json-并统一错误与降级) / [ADR-021](../ADR.md#adr-021联网检索重设计为-exa-与-deepseek-双提供商三工具体系) / [ADR-022](../ADR.md#adr-022收敛为单工具-runbook双端卡片支持一键导出文本) | `2026-11-26` |

## 编写约定

1. 每篇独立记录三个固定部分：背景、决策、实测数据与结论。
2. 实测数据须写明测试环境、硬件或语料条件，以及优化前后的同口径对照结果。
3. 工程规格与接口字段继续归口各模块 `SPEC.md`、`ARCHITECTURE.md` 与 `API.md`，本目录仅保留优化决策与量化记录。

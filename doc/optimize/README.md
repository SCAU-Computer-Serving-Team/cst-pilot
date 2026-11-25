# 优化记录

本目录按专题记录项目各次性能与体验优化的背景、技术决策、实测数据与结论。每次优化对应一篇独立文档，方案总览与分阶段进度见 [9.3.optimization.md](../rust/plan/9.3.optimization.md) 和 [TASKS.md](../TASKS.md)。

## 专题清单

| 文档 | 优化对象 | 核心改造 | 决策依据 | 日期 |
|---|---|---|---|---|
| [storage-ls-disk.md](storage-ls-disk.md) | `ls` 与 `disk` 存储诊断 | 全仓移除 `WizTree`，改用纯 Rust `ntfs-ng` `$MFT`、无条目上限并行 Win32 遍历与原生存储 IOCTL | [ADR-014](../ADR.md#adr-014移除-wiztree存储诊断改用纯-rust-mft-与-win32-ioctl) | `2026-11-24` |
| [file-search-fff.md](file-search-fff.md) | `fffind` 与 `ffgrep` 文件检索 | 单次基线与单句柄 `PathGuardSession`、多线程并行遍历、相关性打分及 UTF-16 / GB18030 解码 | [ADR-017](../ADR.md#adr-017文件检索采用单次基线单句柄核验与无状态并行多编码引擎) | `2026-11-25` |

## 编写约定

1. 每篇独立记录三个固定部分：背景、决策、实测数据与结论。
2. 实测数据须写明测试环境、硬件或语料条件，以及优化前后的同口径对照结果。
3. 工程规格与接口字段继续归口各模块 `SPEC.md`、`ARCHITECTURE.md` 与 `API.md`，本目录仅保留优化决策与量化记录。

# 提示词

## 初始化
1. 若从未阅读过，按照以下顺序阅读文档，获取关键信息
  1. 基本情况：`doc\SPEC.md`、`README.md`、`doc\README.md`。
  2. 架构、待办与测试：`doc\ARCHITECTURE.md`、`doc\TASKS.md`、`doc\TESTING.md`。
  3. 按任务阅读对应模块目录（`doc\runtime\`、`doc\diagnostics\`、`doc\web\`、`doc\auth\`、`doc\telemetry\`、`doc\release\`）下的 `SPEC.md`、`ARCHITECTURE.md`、`API.md`。
2. 请大概了解 `earendil-works/pi`，本项目基于 Pi 二次开发，但是要遵循 Pi 的工程规范。有充足的文档可供后续阅读。

## 文档规范

1. **直白描述**。不用比喻、拟人、夸张，例如不写「吃掉退路」「白花力气」。
2. **中文母语语序**。一句话说一件事，从句拆成独立句。不写「我们」「大家」，不加程度副词。
3. **结构化优先**。
   - 对比、映射、清单用表格。
   - 步骤用有序列表，并列项用无序列表。
   - 每节先给结论，再给理由。
   - 单段不超过 3 句。
4. **必须保持极高信噪比**，对于未来很可能不再使用的信息，如各种旧实现，历史决策，应当不保留。仅保留对当下和未来有价值的内容。

## 运行

```
pi.cmd
```

调试验证：

```bash
export WSLENV=PI_CODING_AGENT_DIR
export PI_CODING_AGENT_DIR='项目目录'
```

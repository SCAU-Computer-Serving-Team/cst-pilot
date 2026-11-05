# 本地维护说明

- 来源：regress 0.12.0，crates.io 发布包。
- 上游：https://github.com/ridiculousfish/regress
- 上游提交：`aae35dee1596cc9a9caaad2425b088081c09e5d5`。
- 许可证：MIT OR Apache-2.0，原文保留在本目录。
- 工作区通过路径依赖使用此版本；更新时需保留并验证以下修改。

## 修改范围

| 文件 | 修改 |
| --- | --- |
| `src/api.rs` | 新增 `find_utf16_bounded` 和 `MatchLimitError`；按 Unicode 标志选择 UTF-16 或 UCS-2 匹配 |
| `src/classicalbacktrack.rs` | 在原生执行器内扣减预算；所有候选起点、嵌套断言、回溯和字符循环共用一次匹配的预算；反向引用按捕获长度预扣比较成本 |
| `src/parse.rs` | 按模式解析 Unicode 码点转义及代理对；未组成代理对时恢复输入位置；捕获组名称始终按 Unicode 解析 |
| `Cargo.toml` | 独立路径依赖清单，保留上游功能开关和依赖 |

预算耗尽时同步停止匹配并返回错误。编译、输入编码和捕获结果分配不计入执行预算。每步成本仍受编译后模式大小影响。

## 验证

`crates/pi-agent/tests/regex_execution.rs` 覆盖上游匹配结果、UTF-16 语义、执行预算和灾难性回溯。仓库根目录运行：

```powershell
cargo test --locked -p pi-agent --test regex_execution
```

`tests/fixtures/ecma_regex.json` 的预期结果来自 `pi-schema@1.5.0`。常规测试直接读取固定结果，无需 Node 环境。

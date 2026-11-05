# QuickJS 时区补丁

来源：crates.io `rquickjs-sys 0.14.0`。保留上游许可证。

- `quickjs/quickjs.c` 增加 `JS_SetThreadTimezoneOffset(enabled, minutes)`。覆盖值保存在 C 线程局部存储中，默认使用系统时区。
- Codemode 为每次执行创建专用线程。确定性执行在线程内启用 UTC，日期构造、解析和格式化共用原生日期逻辑。
- `build.rs` 监听 QuickJS 源文件变化，确保修改后重建。

升级依赖时保留上述补丁，并运行 `codemode_lifecycle` 集成测试。

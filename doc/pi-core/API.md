# Pi 通用核心接口

本文列出 `pi-ai`、`pi-agent`、`pi-session` 与 `pi-runtime` 供上层业务与宿主调用的核心公开类型、Trait 与入口函数。行为约束见 [SPEC.md](SPEC.md)。

## 模型与凭据接口（`pi-ai`）

| 接口或类型 | 用途与契约 |
|---|---|
| `Message` / `UserContentBlock` / `ContentBlock` / `AssistantMessageEvent` | 四角色消息（`System` / `User` / `Assistant` / `ToolResult`）、用户输入块（`Text` / `Image`）、助手内容块（`Text` / `Thinking` / `ToolCall`）与流式增量事件。JSON 序列化与上游格式一致 |
| `Models` | 模型与服务商注册表，提供 `set_provider`、`refresh`、`get_auth`、`stream` 与 `complete` |
| `create_provider` / `Provider` | 声明服务商 ID、默认地址、OAuth/APIKey 认证配置、静态模型列表与协议映射 |
| `CredentialStore` | 凭据持久化抽象，隔离明文令牌与模型目录缓存 |
| `calculate_cost` / `is_context_overflow` / `retry_assistant_call` | 按价目表核算 `Usage` 费用、跨服务商上下文溢出识别与排除账单耗尽错误的指数退避重试 |

## 循环与工具接口（`pi-agent`）

| 接口或类型 | 用途与契约 |
|---|---|
| `AgentTool` / `AgentTool::from_typed<P, F, Fut>` | 工具定义与强类型构造器。`P: DeserializeOwned + JsonSchema` 自动生成参数 Schema，并在运行期先做三阶段宽松转换再反序列化 |
| `AgentToolResult` | 工具返回结构：`{ content, details, structuredContent?, usage?, isError, terminate? }` |
| `AgentEvent` | 生命周期流事件（`snake_case` 类型标签配 `camelCase` 字段）：`agent_start`、`turn_start`、`message_start/update/end`、`tool_execution_start/update/end`、`turn_end`、`agent_end` |
| `validate_tool_arguments` | 对工具参数执行预编译快路径校验，失败时生成带点分路径的可读解释 |
| `run_tool_call` | 单次工具预检、计时执行与前后置钩子封装，供嵌套工具调用复用 |
| `Agent` | 有状态执行容器，提供 `prompt`、`continue_run`、`steer`、`follow_up`、`abort`、`wait_for_idle`、`reset` 与 `subscribe` |

## 会话与静态扩展接口（`pi-session`）

| 接口或类型 | 用途与契约 |
|---|---|
| `SessionStore` | 会话底层存储 Trait（`load_entries(&mut self)`、`rewrite_all`、`append_entry`、`is_poisoned`、`last_tail_repair`）。内置 `InMemorySessionStore` 与 `JsonlSessionStore` |
| `ExecutionStateStore` / `reconcile_on_startup` | 输入投递状态（`Queued / Delivering / Accepted / Failed / Uncertain`）与运行中断日志持久化及启动三步核对 |
| `SessionDataPolicy` | 可选安全策略 Trait：`admit_input`、`persisted_input`、`persisted_message`、`project_event` |
| `SessionManager` | 管理 11 类 `SessionEntry`、`build_session_context` 模型投影、`get_tree` / `build_filtered_tree` 五档过滤树、`branch_with_summary` 与 `fork_branch_to_new_manager` |
| `Extension` / `ExtensionRegistrar` | 编译期静态扩展注册入口。支持注册工具（五级 `ToolExposure`）、斜杠命令与快捷键，同时支持注册服务商、虚拟模型与全生命周期钩子 |
| `ExtensionUiContext` | 与具体 UI 框架解耦的异步交互协议（阻塞弹窗含 `select`、`confirm`、`input`（支持 `secret: true`）、`editor` 与 `custom`，以及编辑器文本与工作状态通知接口） |
| `AgentSession` | 会话主控制器，提供 `prompt`、`steer`、`compact`、`navigate_tree`、`reload` 与 `shutdown` |

## 运行时 SDK 与无头协议（`pi-runtime`）

| 接口或入口 | 用途与契约 |
|---|---|
| `FileAuthStorage` / `ReadOnlyAuthStorage` / `InMemoryAuthStorage` / `RuntimeCredentials` | 文件锁凭据读写、只读凭据视图、内存测试凭据存储与运行期凭据聚合句柄 |
| `SettingsManager` / `ConfigValueResolver` / `ModelRuntime` / `DefaultResourceLoader` | 全局与项目级设置合并（含信任过滤）、`!command` 与环境变量解析、共享模型目录与登录调度、技能／提示词模板／上下文文件加载 |
| `create_all_tool_definitions` / `create_read_tool_definition` 等 | 8 个内置编码工具的定义构造工厂，供宿主按白名单组装或同名覆盖 |
| `create_agent_session_services` | 按工作目录与配置目录初始化 `ModelRuntime`、`SettingsManager` 与 `DefaultResourceLoader`，返回 `AgentSessionServices` |
| `create_agent_session` | 组装内置工具、扩展、资源、数据策略与 `AgentSession`，支持 `InitialModelPolicy::{GenericSdk, FixedIdentity}` |
| `AgentSessionRuntime` | 单写者跨会话切换、新建、派生、JSONL 导入与有序销毁宿主 |
| `bin/pi.rs`（Print / JSON / RPC） | 命令行无头入口：单次文本打印、剥离 `partial` 大快照的 JSON 事件流，以及标准输入输出异步解耦的 JSON-RPC 会话控制模式 |

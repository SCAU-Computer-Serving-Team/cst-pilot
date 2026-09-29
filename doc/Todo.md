# Todo

## 开发准备

1. [ ] 准备统一模型配置入口的需求文档
2. [ ] 按[遥测需求](telemetry/README.md#需求)与规格实现采集和上报；OAuth 依赖按该目录的待决项处理
3. [ ] **自制 @cst-pilot/web**：后端接口、会话池、输入收件箱与并发隔离已完成本机联调；首页、聊天工作台、设置与登录页已接入接口。接下来按 [MVP](web/MVP.md) 补齐分支树与视觉对照；功能完成后接入发行包。
4. [ ] `SPEC` 与详细设计的目录分类暂缓，见[文档分类](issues.md#d4-规格与详细设计的分类)。
5. [ ] 账号菜单按画布 `n26gq` 重组：Provider 登录态并入账号信息、补分隔线。先做前端。Provider 状态应当等价于 `/scope-model` 这类命令的作用面——当前模型选择器把全部模型都列进来，超出预期。
6. [ ] cstoa 额度接口待确定。DeepSeek 余额与 OpenCode Go / Go Plus 用量已接入上下文面板，见 [聊天工作台](web/SPEC/chat-workspace.md)。

## ToDraw 画布

`src/web/design/cst-pilot-web.pen` 现有：主页、聊天工作台、登录页、仪表盘各浅深两版；APIKEY 默认态、自定义 Provider 态和 Provider 下拉各浅深两版；上下文面板、模型选择、思考强度、账号菜单、消息菜单的悬停态；输入框的补全面板、补全选中标记与排队组件（基础、暂停、失败）各浅深两版。`src/web/design/cst-pilot-tools.pen` 现有：三种通用渲染模式、折叠层级、等待/成功/失败/降级等状态、逐工具示例，以及 `sys overview`、`runbook`、`web_search`、`read` 图片等例外。下面只列尚需补画或调整的内容。

1. [ ] 设置视图 `/settings`
2. [ ] 工具卡片补齐，画布 `src/web/design/cst-pilot-tools.pen`：
   - [ ] 多列排行行：名称 + 2～3 个右对齐指标列
   - [ ] 输出截断提示条：呈现 `outputTruncated` 等裁剪信息；现有画布已有 `notice`、降级和失败示例
3. [ ] 图片缩略图条：横向滚动、hover 出删除、点击看大图，见 [聊天工作台](web/SPEC/chat-workspace.md#图片)
4. [ ] 扩展提问面板：选择、确认、输入、多行编辑四种，见 [会话运行与并行](web/SPEC/session-runtime.md#能力范围)
5. [ ] 操作栏对齐与补齐：按[命令与功能对应](web/SPEC/commands.md)补复制、导出与派生入口；MVP 不显示分享菜单
6. [ ] 断线横幅与明确的退出入口，见 [前端工程](web/SPEC/frontend.md#用户可见的约束)
7. [ ] 聊天工作台的对话流帧要重画：把 `cst-pilot-tools.pen` 中已有的折叠层级和展开卡片接入完整对话场景，替换当前「已完成工作」示意

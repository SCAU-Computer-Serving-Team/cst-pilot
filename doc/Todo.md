# Todo

## 开发准备

1. [ ] 准备统一模型配置入口的需求文档
2. [ ] 按[遥测需求](telemetry/README.md#需求)与规格实现采集和上报；OAuth 依赖按该目录的待决项处理
3. [ ] **自制 @cst-pilot/web**：规格草案与主要页面原型已有；官方二进制已验证最小 Web 会话与 TUI 待机接管。下一步补齐[工程验证](web/issues.md)，按 [MVP](web/MVP.md) 实现；功能完成后接入发行包。
4. [ ] `SPEC` 与详细设计的目录分类暂缓，见[文档分类](issues.md#d4-规格与详细设计的分类)。

## ToDraw 画布

`src/web/design/cst-pilot-web.pen` 现有：主页、聊天工作台、登录页、仪表盘各浅深两版；上下文面板、模型选择、思考强度、账号菜单、消息菜单的悬停态。`src/web/design/cst-pilot-tools.pen` 现有：「聊天工作台 · 浅色」、工具卡片的三种通用渲染模式、「已发送（等待返回）」状态、`sys overview` 与 `runbook` 两个例外。缺的列在下面。

1. [ ] 分支树视图 `/s/<id>/tree`，见 [命令与功能对应](web/SPEC/commands.md)
2. [ ] 输入框补全面板：`/` 命令与 `@` 文件引用，含选中后折叠成标记的样子，见 [命令与功能对应](web/SPEC/commands.md#输入框的交互)
3. [ ] 排队与插队组件：聊天框上方的弹出组件，带插话、编辑、删除、拖拽排序，见 [聊天工作台](web/SPEC/chat-workspace.md#排队与插队)
4. [ ] 设置视图 `/settings`
5. [ ] 工具卡片补齐，画布 `src/web/design/cst-pilot-tools.pen`：
   - [ ] 多列排行行：名称 + 2～3 个右对齐指标列
   - [ ] 提示条：图标 + 文案，放 `notice` / `degraded` / `truncated`
   - [ ] `isError` 失败态
   - [ ] 联网检索工具的结果卡（`web_search` / `source_check` / `fetch_content` / `get_search_content`）
6. [ ] 图片缩略图条：横向滚动、hover 出删除、点击看大图，见 [聊天工作台](web/SPEC/chat-workspace.md#图片)
7. [ ] 扩展提问面板：选择、确认、输入、多行编辑四种，见 [会话运行与并行](web/SPEC/session-runtime.md#能力范围)
8. [ ] 操作栏对齐与补齐：按[命令与功能对应](web/SPEC/commands.md)补复制、导出与派生入口；MVP 不显示分享菜单
9. [ ] 断线横幅与明确的退出入口，见 [前端工程](web/SPEC/frontend.md#用户可见的约束)
10. [ ] 聊天工作台的对话流帧要重画：现在画的是「已完成工作」折叠行加正文表格，没有工具卡片展开后的样子
11. [ ] 补登录页的 API KEY 态（浅深两版）；扫码 OAuth 态留待后续阶段

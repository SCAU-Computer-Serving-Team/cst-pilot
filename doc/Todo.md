# Todo

更新：2026-10-02。

## 功能开发

1. [ ] **自制 @cst-pilot/web**：按 [MVP](web/MVP.md) 推进，当前阶段为页面与画布的视觉对照；功能验收后接入发行包
2. [ ] 遥测：按[遥测需求](telemetry/README.md#需求)与规格实现采集和上报；OAuth 依赖按该目录的待决项处理
3. [ ] 统一模型配置入口的需求文档
4. [ ] 账号菜单按画布 `n26gq` 重组：Provider 登录态并入账号信息、补分隔线。先做前端
5. [ ] cstoa 额度接口待确定；DeepSeek 余额与 OpenCode Go / Go Plus 用量已接入上下文面板，见 [聊天工作台](web/SPEC/chat-workspace.md)
6. [ ] `SPEC` 与详细设计的目录分类暂缓，见[文档分类](issues.md#d4-规格与详细设计的分类)

## 开源与工程化

1. [ ] 补 LICENSE：仓库根目录缺许可证文件，开源发布前必须补
2. [ ] GitHub Actions CI：push 与 PR 触发，运行 `agent` 的 `check` 与 `test:web`、前端的 typecheck 与测试；未通过不合并
3. [ ] 前端 lint：biome 配置扩展到 `src/web/frontend`，与 typecheck、测试互补
4. [ ] 贡献规范：CONTRIBUTING 说明本地开发、检查与测试命令；issue 模板区分缺陷报告与议题讨论
5. [ ] 发行接入校验：静态产物存在性、文件清单与第三方许可证，见 [MVP 后续安排](web/MVP.md#六后面再做)

## ToDraw 画布

对照日期：2026-10-02，对照方式为逐帧比对画布与前端实现。

现有画布：[cst-pilot-web.pen](../src/web/design/cst-pilot-web.pen) 覆盖主页、聊天工作台、登录页（OAuth 与 APIKEY 各态）、仪表盘、分支树、分支总结四态、排队与插队各态（含暂停、失败、失败悬停）、补全面板（命令/文件/选中标记）、未发送图片、账号与消息菜单的浅深两版；[cst-pilot-tools.pen](../src/web/design/cst-pilot-tools.pen) 覆盖工具卡片的三个模板（概览组、排行组、原文块，各浅深）、折叠层级、等待/成功/失败/降级/回退各态、多列排行行与逐工具示例（含联网检索与 read 图片例外）。下面只列尚需补画或调整的内容。

1. [ ] 对话流帧重画：把 `cst-pilot-tools.pen` 已有的折叠层级和展开卡片接入完整对话场景，替换当前「已完成工作」示意；过程折叠行（工作中 N 秒）与等待流光还没有画布，见[聊天工作台](web/SPEC/chat-workspace.md#过程折叠)
2. [ ] 输出截断提示条：呈现 `outputTruncated` 等裁剪信息；画布已有 `notice`、降级、回退与失败示例，多列排行行已补
3. [ ] 扩展提问面板画布：选择、确认、输入、多行编辑四种；前端功能已实现（原生控件，无样式对照），见 [会话运行与并行](web/SPEC/session-runtime.md#能力范围)
4. [ ] 消息操作栏画布：复制、派生、导出与时间前端已实现，画布缺；MVP 不显示分享菜单，见[命令与功能对应](web/SPEC/commands.md)
5. [ ] 断线横幅与明确的退出入口：前端有基础横幅，画布与退出入口设计缺，见 [前端工程](web/SPEC/frontend.md#用户可见的约束)
6. [ ] 分支树补齐与 TUI 对齐，见[分支树](web/SPEC/branch-tree.md)：
   - [ ] 视图过滤四档（无工具、仅用户、仅标记、全部）与默认档持久化；前端为禁用占位，画布未画
   - [ ] 条目标记（label）的编辑入口与标记时间戳显示
   - [ ] 条目复制（TUI 的 c 键）
   - [ ] 键盘跳段（TUI 的 h/l 键在段首间移动）

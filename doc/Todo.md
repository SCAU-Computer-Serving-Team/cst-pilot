# Todo

更新：2026-10-03。

## 功能开发

1. [ ] **自制 @cst-pilot/web**：按 [MVP](web/MVP.md) 推进，当前阶段为页面与画布的视觉对照；功能验收后接入发行包
2. [x] 遥测：sender（`agent/home/extensions/telemetry/`）与 receiver（[cst-pilot-server](https://github.com/SCAU-Computer-Serving-Team/cst-pilot-server)）已实现并经本机 e2e 验证；receiver 已部署 timserver_1（桩模式，公网入口与定时任务待接）；待做：发行白名单加 `telemetry.json`、OAuth 接入后换真内省
3. [ ] OAuth 扩展：规格已定稿，骨架与交接文档就绪（[doc/auth/handover.md](auth/handover.md)），待接手人开发
3. [ ] 统一模型配置入口的需求文档
4. [ ] cstoa 额度接口待确定；DeepSeek 余额与 OpenCode Go / Go Plus 用量已接入上下文面板，见 [聊天工作台](web/SPEC/chat-workspace.md)
5. [ ] `SPEC` 与详细设计的目录分类暂缓，见[文档分类](issues.md#规格与详细设计的分类)
6. [ ] 补全面板按新画布重画（`x62krm`/`oBWkc` 浅深）：定宽 400 左对齐悬浮、名称定宽列 + 说明紧邻、选中行底色、底部快捷键提示


## ToDraw 画布

对照日期：2026-10-02，对照方式为逐帧比对画布与前端实现。

现有画布：[cst-pilot-web.pen](../src/web/design/cst-pilot-web.pen) 覆盖主页、聊天工作台、登录页（OAuth 与 APIKEY 各态）、仪表盘、分支树、分支总结四态、消息弹窗（浅深）、排队与插队各态（含暂停、失败、失败悬停）、补全面板与选中标记、派生选择（浅深）、未发送图片、账号与消息菜单的浅深两版；[cst-pilot-tools.pen](../src/web/design/cst-pilot-tools.pen) 覆盖工具卡片的三个模板（概览组、排行组、原文块，各浅深）、折叠层级、等待/成功/失败/降级/回退各态、多列排行行与逐工具示例（含联网检索与 read 图片例外）。下面只列尚需补画或调整的内容。

1. [ ] 扩展提问面板画布：选择、确认、输入、多行编辑四种；前端功能已实现但用原生控件，与全站 HeroUI 不一致，统一组件后补一帧定布局，见 [会话运行与并行](web/SPEC/session-runtime.md#能力范围)
2. [ ] 断线横幅与明确的退出入口：前端有基础横幅但颜色未走语义令牌，画布与退出入口设计缺，见 [前端工程](web/SPEC/frontend.md#用户可见的约束)
3. [ ] 分支树补齐与 TUI 对齐（MVP 后），见[分支树](web/SPEC/branch-tree.md)：
   - [ ] 视图过滤下拉的弹层画布（四档功能与 Ctrl+O 已落地，见 SPEC）
   - [ ] 条目标记（label）的编辑入口与标记时间戳显示
   - [ ] 条目复制（TUI 的 c 键）
   - [ ] 键盘跳段（TUI 的 h/l 键在段首间移动）

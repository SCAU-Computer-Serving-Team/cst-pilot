# Todo

更新：2026-10-05。Web 阶段范围与验收见 [MVP](web/MVP.md)，待决问题见[议题清单](issues.md)。

## 功能与资源

| 待办 | 依据 |
|---|---|
| Checkpoint 5：实际窗口、输入法、网络与 TUI 待机联调 | [MVP](web/MVP.md#八实施-checkpoint) |
| 功能 MVP 验收后接入便携发行包，执行发行副本冒烟 | [前端设计](design/web/frontend.md#打包接入) |
| 遥测接收端增加 rollup、cleanup 定时任务，并使用真实 OAuth 内省 | [遥测](telemetry/) |
| OAuth 真机联调；OA 提供 `/api/oauth/introspect` | [认证交接](auth/handover.md) |
| 接入 cstoa 学号、姓名的 Agent 资料接口与额度查询 | [专属账号](web/SPEC/app-router.md#专属账号) |

## 待补画布

现有画布与定位方法见 [DESIGN.md](../DESIGN.md#画布定位)。本节只列待补内容。

| 待补项 | 阶段与依据 |
|---|---|
| 断线横幅、明确的退出入口；退出前需先确定运行中任务的处理 | Checkpoint 5，[前端工程](web/SPEC/frontend.md#用户可见的约束) |
| 分支树视图过滤下拉的弹层；五档过滤与 Ctrl+O 已实现 | MVP 后，[分支树](web/SPEC/branch-tree.md#可见条目) |
| 分支树标记编辑入口、标记时间、条目复制与键盘跳段 | MVP 后，[分支树](web/SPEC/branch-tree.md) |

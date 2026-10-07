# Todo

更新：2026-10-07。Web 阶段范围与验收见 [MVP](web/MVP.md)，待决问题见[议题清单](issues.md)。

## 功能与资源

| 待办 | 依据 |
|---|---|
| Checkpoint 5：待机、断线、失效提示与退出已完成本机隔离回归；系统输入法、真实粘贴拖拽与最低版本继续验证 | [计划与报告](test/checkpoint5-report.md)、[MVP](web/MVP.md#八实施-checkpoint) |
| prev0.5试用包：保留官方exe与白名单装配，新增当前Web构建/字体检查与实际Web发行冒烟；跨机/介质待验证 | [可行性分析](web/research/release-feasibility.md)、[前端设计](design/web/frontend.md#打包接入) |
| 遥测接收端增加 rollup、cleanup 定时任务，并使用真实 OAuth 内省 | [遥测](telemetry/) |
| OAuth 真机联调；OA 提供 `/api/oauth/introspect` | [认证交接](auth/handover.md) |
| 接入 cstoa 学号、姓名的 Agent 资料接口与额度查询 | [专属账号](web/SPEC/app-router.md#专属账号) |

## 待补画布

现有画布与定位方法见 [DESIGN.md](../DESIGN.md#画布定位)。本节只列待补内容。

| 待补项 | 阶段与依据 |
|---|---|
| 错误与退出浅深组件已补至09区；继续按实际窗口验收 | Checkpoint 5，[前端工程](web/SPEC/frontend.md#用户可见的约束) |
| 分支树视图过滤下拉的弹层；五档过滤与 Ctrl+O 已实现 | MVP 后，[分支树](web/SPEC/branch-tree.md#可见条目) |
| 分支树标记编辑入口、标记时间、条目复制与键盘跳段 | MVP 后，[分支树](web/SPEC/branch-tree.md) |

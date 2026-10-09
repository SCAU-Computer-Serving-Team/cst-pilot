# Todo

更新：2026-10-07。Web 阶段范围与验收见 [MVP](web/MVP.md)，待决问题见[议题清单](issues.md)。

## 功能与资源

| 待办 | 依据 |
|---|---|
| Checkpoint 5：待机、断线、失效提示与退出已完成本机隔离回归；系统输入法、真实粘贴拖拽与最低版本继续验证 | [计划与报告](test/checkpoint5-report.md)、[MVP](web/MVP.md#八实施-checkpoint) |
| 共享后端：本机修复回归完成；继续补离线目录独立变更、全部技能/附件组合、多用户和目标系统验证 | [共享联调报告](test/shared-backend-report.md) |
| v0.5发行包：官方exe、白名单装配、TUN假IP放行、当前Web构建/字体检查与实际Web发行冒烟；跨机/介质待验证 | [可行性分析](web/research/release-feasibility.md)、[前端设计](design/web/frontend.md#打包接入) |
| 遥测真实队员双端入库验收；两端异机备份与完整恢复验证；清理和汇总监控 | [遥测验证](test/telemetry-report.md) |
| OAuth 真机联调；验证真实登录、模型调用与遥测身份 | [认证交接](auth/handover.md) |
| OA 发布 Agent 本人资料接口并完成生产账号展示验收；额度查询待接入 | [专属账号](web/SPEC/app-router.md#专属账号) |

## 待补画布

现有画布与定位方法见 [DESIGN.md](../DESIGN.md#画布定位)。本节只列待补内容。

| 待补项 | 阶段与依据 |
|---|---|
| 错误与退出浅深组件已补至09区；继续按实际窗口验收 | Checkpoint 5，[前端工程](web/SPEC/frontend.md#用户可见的约束) |
| 分支树视图过滤下拉的弹层；五档过滤与 Ctrl+O 已实现 | MVP 后，[分支树](web/SPEC/branch-tree.md#可见条目) |
| 分支树标记编辑入口、标记时间、条目复制与键盘跳段 | MVP 后，[分支树](web/SPEC/branch-tree.md) |

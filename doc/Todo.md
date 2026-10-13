# Todo

更新：2026-10-13。Web 阶段范围与验收见 [MVP](web/MVP.md)，待决问题见[议题清单](issues.md)。

## 功能与资源

| 待办 | 依据 |
|---|---|
| Checkpoint 5：待机、断线、失效提示与退出已完成本机隔离回归；系统输入法、真实粘贴拖拽与最低版本继续验证 | [计划与报告](test/checkpoint5-report.md)、[MVP](web/MVP.md#八实施-checkpoint) |
| 共享后端：本机修复回归完成；继续补离线目录独立变更、全部技能/附件组合、多用户和目标系统验证 | [共享联调报告](test/shared-backend-report.md) |
| 当前源码发行验收：重建干净包，验证管理员启动、Web 资源与实际工具链路；跨机、介质和杀毒软件待验证 | [发行方案](design/release.md)、[前端设计](design/web/frontend.md#打包接入) |
| 管理员与 WT 启动：真实 UAC 同意／取消、跨账户授权、未安装 WT 和系统控制台回退 | [运行边界](Notice.md#管理员启动)、[议题清单](issues.md#实机验证) |
| 会话管理、通知与简化模型面板：补最低浏览器、实际窗口和多标签组合验收 | [端到端验收](test/web-e2e.md)、[联调报告](test/checkpoint5-report.md) |
| 遥测真实队员双端入库验收；两端异机备份与完整恢复验证；清理和汇总监控 | [遥测验证](test/telemetry-report.md) |
| OAuth 真机联调；验证真实登录、模型调用与遥测身份 | [认证交接](auth/handover.md) |
| CSTOA 资料与上下文额度的多队员生产验收 | [账号验证](test/account-report.md) |

## 待补画布

现有画布与定位方法见 [DESIGN.md](../DESIGN.md#画布定位)。本节只列待补内容。

| 待补项 | 阶段与依据 |
|---|---|
| 分支树视图过滤下拉的弹层；五档过滤与 Ctrl+O 已实现 | MVP 后，[分支树](web/SPEC/branch-tree.md#可见条目) |
| 分支树标记编辑入口、标记时间、条目复制与键盘跳段 | MVP 后，[分支树](web/SPEC/branch-tree.md) |

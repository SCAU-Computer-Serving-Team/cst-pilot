# Todo

更新：2026-10-15。Web 阶段范围与验收见 [MVP](web/MVP.md)，待决问题见[议题清单](issues.md)。

## 功能与资源

| 待办 | 依据 |
|---|---|
| 遥测发行验收：补真实 U 盘跨机、只读介质、强杀与磁盘故障组合；验证真实队员上传和长期重试 | [可靠性验证](test/telemetry-report.md#可靠性缺口) |
| BitLocker 实机验收：工具、TUI、Web 与 SKILL 已接入；补实际终端交互和隔离加密卷的错误密钥、自动解锁依赖、暂停恢复、部分失败及超时；覆盖 Windows 10/11 与 PE | [TUI](tool/bitlocker.md#tui-交互)、[执行规则](tool/bitlocker.md#decrypt完整解密)、[测试入口](../src/test/bitlocker.test.mjs) |
| BitLocker 凭据边界：补模型请求、Windows 审计、终端录制及其他扩展的实测；AI 填参的 Web 草稿与收件箱原文、图像和附件脱敏待完善 | [凭据边界](tool/bitlocker.md#凭据与-skill)、[执行限制](tool/bitlocker.md#限制) |
| 会话导出与分享：消息菜单仅展示置灰入口，格式、附件范围、脱敏与分享规则待设计 | [命令规格](web/SPEC/commands.md)、[分享](web/SPEC/session-runtime.md#分享) |
| Checkpoint 5：待机、断线、失效提示与退出已完成本机隔离回归；系统输入法、真实粘贴拖拽与最低版本继续验证 | [计划与报告](test/checkpoint5-report.md)、[MVP](web/MVP.md#八实施-checkpoint) |
| 共享后端：本机修复回归完成；继续补离线目录独立变更、全部技能/附件组合、多用户和目标系统验证 | [共享联调报告](test/shared-backend-report.md) |
| 当前源码发行验收：重建干净包，验证管理员启动、Web 资源与实际工具链路；跨机、介质和杀毒软件待验证 | [发行方案](design/release.md)、[前端设计](design/web/frontend.md#打包接入) |
| 管理员与 WT 启动：真实 UAC 同意／取消、跨账户授权、未安装 WT 和系统控制台回退 | [运行边界](Notice.md#管理员启动)、[议题清单](issues.md#实机验证) |
| 会话管理、通知与简化模型面板：补最低浏览器、实际窗口和多标签组合验收 | [端到端验收](test/web-e2e.md)、[联调报告](test/checkpoint5-report.md) |
| 遥测真实队员双端入库验收；两端异机备份与完整恢复验证；清理和汇总监控 | [遥测验证](test/telemetry-report.md) |
| OAuth 真机联调；验证真实登录、模型调用与遥测身份 | [CSTOA 接入](auth/integration.md) |
| CSTOA 资料与上下文额度的多队员生产验收 | [账号验证](test/account-report.md) |

## 待补画布

现有画布与定位方法见 [DESIGN.md](../DESIGN.md#画布定位)。本节只列待补内容。

| 待补项 | 阶段与依据 |
|---|---|
| 分支树视图过滤下拉的弹层；五档过滤与 Ctrl+O 已实现 | MVP 后，[分支树](web/SPEC/branch-tree.md#可见条目) |
| 分支树标记编辑入口、标记时间、条目复制与键盘跳段 | MVP 后，[分支树](web/SPEC/branch-tree.md) |

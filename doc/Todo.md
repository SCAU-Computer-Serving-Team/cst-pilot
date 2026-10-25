# 后续待办

**MVP 阶段已完成。** 本页只列后续功能迭代、发行与实机验证、安全和运维任务。已交付范围见 [Web MVP](web/MVP.md)，未决产品选择见[议题清单](issues.md)。

## 功能迭代

| 待办 | 依据 |
|---|---|
| 兼容版：面向 Chromium 86 单独构建样式并验证交互 | [浏览器基线](web/SPEC/frontend.md#浏览器基线) |
| 仪表盘：实现用量概览、趋势、筛选与日志 | [仪表盘](web/SPEC/dashboard.md) |
| 会话导出与分享：确定格式、附件范围、脱敏和分享销毁规则 | [命令规格](web/SPEC/commands.md)、[分享](web/SPEC/session-runtime.md#分享) |
| 会话内搜索与会话列表搜索 | [范围边界](web/MVP.md#七范围边界) |

## 发行与实机验证

| 待办 | 依据 |
|---|---|
| 干净发行包回归：管理员启动、Web 资源和实际工具链路；实际介质、跨机、只读环境与杀毒软件 | [发行方案](design/release.md)、[前端设计](design/web/frontend.md#打包接入) |
| 管理员与 WT：真实 UAC 同意／取消、跨账户授权、未安装 WT 和系统控制台回退 | [启动器测试](test/launcher.md)、[运行边界](Notice.md#管理员启动) |
| Web 目标环境：系统输入法、真实粘贴拖拽、最低浏览器、实际窗口与多标签组合 | [实机验证方法](test/web-e2e.md#实机与发行验证) |
| 共享后端：离线目录独立变更、技能与附件组合、多用户和目标系统 | [共享后端测试](test/shared-backend.md) |
| BitLocker：实际终端、隔离加密卷的错误密钥、自动解锁依赖、暂停恢复、部分失败及超时；覆盖 Windows 10/11 与 PE | [TUI](tool/bitlocker.md#tui-交互)、[执行规则](tool/bitlocker.md#decrypt完整解密)、[测试入口](../src/test/bitlocker.test.mjs) |
| CSTOA：首次设备扫码动态码、生产 APIKEY、多成员资料和额度核对 | [账号测试](test/account.md)、[CSTOA 接入](auth/integration.md) |
| 遥测：真实 U 盘跨机、写保护、强杀、磁盘故障与长期重试组合 | [实机验证方法](test/telemetry.md#真实服务与环境验证) |

## 安全与运维

| 待办 | 依据 |
|---|---|
| BitLocker 凭据边界：模型请求、Windows 审计、终端录制与其他扩展实测；完善 Web 草稿、收件箱、图像与附件脱敏 | [凭据边界](tool/bitlocker.md#凭据与-skill)、[执行限制](tool/bitlocker.md#限制) |
| 发布 CSTOA 请求级扣点能力并核对真实账单 | [遥测部署](telemetry/deployment.md#cstoa-扣点能力发布)、[遥测测试](test/telemetry.md) |
| 遥测多成员上传、两端异机备份与完整恢复、清理和汇总监控 | [遥测运维](issues.md#遥测运维) |

## 待补画布

现有画布与定位方法见 [DESIGN.md](../DESIGN.md#画布定位)。

| 待补项 | 依据 |
|---|---|
| 分支树视图过滤下拉的弹层 | [分支树](web/SPEC/branch-tree.md#可见条目) |
| 分支树标记编辑入口、标记时间、条目复制与键盘跳段 | [分支树](web/SPEC/branch-tree.md) |

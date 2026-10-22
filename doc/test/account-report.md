# CSTOA 账号与额度验证

## 结论

OAuth 与 APIKEY 的本人资料、积分、自动模型和退出通过本机隔离验收。服务端 APIKEY 资料接口已在 [OA PR #131](https://github.com/SCAU-Computer-Serving-Team/cstoa-api-fastapi/pull/131) 合并并部署；健康、就绪、新接口和既有 OAuth 接口的无凭据拒绝检查通过。

真实手机 2FA、生产多队员资料和实际积分扣减仍需使用获准的测试账号验证。

## 本机验证

日期：2026-10-22。Windows 环境，真实 Pi SDK 与隔离 Chrome。OA 和模型请求使用本地替身，临时凭据、截图和浏览器数据保存在日期临时目录，不访问用户 Cookie。

| 范围 | 结果 |
|---|---|
| 扩展 Biome 与 TypeScript | 通过 |
| 前端 lint、类型与构建 | 通过 |
| OAuth 与动态目录 | 13 项通过 |
| Web 后端 | 105 项通过，含账号、授权、模型与独立 Pi 进程恢复 |
| 共享配置专项 | 7 项通过 |
| 遥测 | 20 项通过 |
| 前端单元与字体 | 164 项与 4 项通过 |
| 工具与文档约束 | 38 项通过 |
| 发行脚本 | 36 项通过 |
| 全量浏览器验收 | 23 项通过；账号和 APIKEY 登录覆盖 1440×1000 浅色与 480×900 深色 |
| 工程静态检查 | Biome 通过；本机未取得完整 PSScriptAnalyzer 1.24.0，PowerShell 检查由 CI 执行 |

## 覆盖

| 场景 | 验证结果 |
|---|---|
| 双方式推理 | 真实 Pi 通过 OAuth 代理与 APIKEY 直连入口取得流式回复，凭据和请求地址分开 |
| 身份与积分 | 两种方式共用账号及额度接口；账号页、设置和菜单展示身份，上下文展示积分 |
| 密钥登录 | 账号页进入 APIKEY 表单，固定地址选项禁用；完成后回到账号页，侧栏可退出 |
| 模型 | 登录后获取、跨进程持久化恢复、自动增删、成功空目录、失败缓存与重试退避；读取有效缓存不重写共享文件 |
| 凭据隔离 | 换凭据不沿用旧目录；失败登录保留已有凭据；退出只清 CSTOA |
| 积分更新 | 请求结束后刷新缓存；0 是有效值；资料和积分故障分别处理 |
| 安全 | 目录不含凭据；浏览器存储不含 APIKEY；上游模型地址和请求头不能改变连接配置 |
| 布局 | 两种窗口与主题、长姓名、面板边界、键盘打开及 Esc 关闭通过 |

## 可重复入口

```powershell
npm run check --prefix agent
npm run test:oauth --prefix agent
npm run test:web --prefix agent
npm run test:web:shared-audit --prefix agent
npm run test:telemetry --prefix agent
node --test src/test/*.test.mjs
node --test pack/test/*.test.mjs
npm run lint --prefix src/web/frontend
npm run typecheck --prefix src/web/frontend
npm run test --prefix src/web/frontend
npm run build --prefix src/web/frontend
npm run test:fonts --prefix src/web/frontend
npm run test:web:e2e --prefix agent
```

截图位于日期临时目录的 `account-profile/` 与 `cstoa-key-login/`，不进入 Git。

## 服务端发布

| 项 | 结果 |
|---|---|
| 后端自检 | 相关测试 124 项及 12 项子测试通过，Ruff 通过 |
| CI | 6 个任务全部通过，包含完整 SQLite、MySQL 集成、锁文件、安全检查和容器构建 |
| 部署范围 | 当前生产基线 `df075086a137` 加本次 4 个 Python 文件；不改数据库、New API、nginx 或环境配置 |
| 发布后复核 | 服务为 active，4 个 Python 文件摘要与增量清单一致；健康、就绪及两类接口的无凭据拒绝通过，合成无效 APIKEY 被拒绝 |
| 发布目录 | `/opt/cstoa-api-fastapi-dual/releases/df075086a137-pr131-9761216bfd3e` |
| 回退目录 | `/opt/cstoa-api-fastapi-dual/releases/df075086a137`；切回 `current` 后重启 `cstoa.service` |

## 未验证边界

真实手机 2FA、生产多队员资料和实际积分扣减仍需使用获准的测试账号验证。完整发行装配与管理员硬件冒烟未执行。

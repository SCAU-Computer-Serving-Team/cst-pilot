# CSTOA 账号与额度验证

验证日期：2026-10-09。环境：Windows 11、Node 24.15.0、Chrome 154、模拟 OA 与真实 Pi SDK。配置、凭据和截图均使用隔离目录；未读取用户 Cookie，未访问生产资料或模型接口。

## 结论

客户端采用 OA `GET /api/agent/profile` 契约，OA 侧实现依据 [#124](https://github.com/SCAU-Computer-Serving-Team/cstoa-api-fastapi/pull/124)。账号页、设置与菜单只显示学号和姓名；额度集中在 CSTOA 模型的上下文面板。

账号和额度复用一次上游查询，缓存仅存于内存。正常退出、凭据隔离和遥测行为保持不变。

## 已执行验证

| 范围 | 结果 |
|---|---|
| 后端 Biome 与类型 | 通过 |
| Web 后端测试 | 84 项通过，包含账号、额度接口和真实 Pi 请求结束后的缓存失效 |
| 前端 Biome 与类型 | 通过 |
| 前端测试 | 138 项通过，覆盖身份文案、额度点及零值格式 |
| Web 浏览器全量链路 | 14 项通过，覆盖账号、授权、真实工具、恢复、页面切换和退出 |
| 跨端共享验收 | 7 项通过，使用独立 Pi 进程 |
| OAuth 与遥测回归 | 分别 11 项、14 项通过 |
| 发行检查 | 18 项通过 |
| 画布与文档契约 | 14 项通过，节点 ID、配色、字段分工和链接可用 |
| Web 构建与分发字体 | 构建通过，字体 4 项通过 |
| UI 静态检查 | Impeccable 检查无发现 |

账号浏览器链路覆盖 1440×1000 浅色和 480×900 深色。核对身份展示、设置和菜单无额度、上下文额度、键盘打开与 Esc 关闭、视口边界、零额度、额度故障、重试、模型切换、长姓名和退出清空。第二个隔离浏览器同步收到退出事件并清除额度。

接口测试覆盖 HTTP 401/403、OA 19008、格式错误、网络失败、令牌变化、缓存上限、并发合并和迟到响应。模拟额度服务不可用时，姓名仍可显示；真实 Pi 请求只访问本机模拟模型接口。

截图保存在测试临时目录的 `account-profile/` 下，包含 `account-*.png`、`settings-*.png`、`context-*.png` 与 `menu-dark.png`，不进入 Git。Pencil 未连接，本轮通过本地文件同步设计稿，并完成结构检查和浏览器实渲对照；设计软件内的画布布局复核未执行。

## 可重复入口

```powershell
npm run check --prefix agent
npm run test:web --prefix agent
npm run lint --prefix src/web/frontend
npm run typecheck --prefix src/web/frontend
npm run test --prefix src/web/frontend
npm run build --prefix src/web/frontend
npm run test:web:e2e --prefix agent
```

局部链路：`node --test agent/home/extensions/web/test/account.e2e.ts`。跨端链路：`npm run test:web:shared-audit --prefix agent`。

## 未验证边界

生产多队员、实际扣减后的额度、移动介质和最低浏览器版本仍待验收。遥测双端真实入库与恢复验证见[遥测验证](telemetry-report.md)。本轮不替用户扫码授权，不修改或重启生产服务。

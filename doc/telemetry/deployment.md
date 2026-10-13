# 遥测部署

同款 Go 接收端分别部署在 CSTOA 与 Tim 的 `timserver_1`，数据库和确认状态独立。接口与存储规则见[接收端规格](receiver/SPEC.md)，验证结果见[遥测验证](../test/telemetry-report.md)。

## 服务配置

服务使用环境变量配置：

```
TELEMETRY_PORT=8787
TELEMETRY_HOST=127.0.0.1
TELEMETRY_DB=/var/lib/cst-telemetry/telemetry.db
OA_INTROSPECT_URL=http://127.0.0.1:8080/api/oauth/introspect   # 生产必须配置真实内省
OA_SERVICE_TOKEN=<接收端服务凭据>
```

OA 使用 `CSTOA_OA_INTROSPECT_SERVICE_TOKEN` 校验内省调用方，接收端通过 `OA_SERVICE_TOKEN` 发送对应服务凭据。服务凭据只保存在服务器，不进入工具包。

## CSTOA 接收端

运行于 CSTOA 服务器，与 OA、nginx 共存。

| 项 | 实际值 |
|---|---|
| 二进制与 env | `/opt/cst-pilot-server/telemetry-receiver`、同目录 `telemetry.env`（root:csttele） |
| 数据库 | `/var/lib/cst-telemetry/telemetry.db`，属主系统用户 `csttele`（无 shell） |
| 服务 | systemd `cst-telemetry.service`，`ProtectSystem=strict` + `ReadWritePaths=/var/lib/cst-telemetry`，开机自启 |
| 内部监听 | `127.0.0.1:8787`，路径 `POST /v1/sessions`、`GET /healthz` |
| 公网入口 | nginx：`location = /api/telemetry` → `127.0.0.1:8787/v1/sessions` |
| 上报地址 | `https://www.cstoa.top/api/telemetry`。裸域 cstoa.top 301 到 www，POST 跟随重定向会丢包，endpoint 必须带 www |
| 身份 | 真实内省，地址 `http://127.0.0.1:8080/api/oauth/introspect`；OA 与接收端服务凭据一致 |
| 内存 | 常驻约 11MB |

当前验证范围见[遥测验证](../test/telemetry-report.md)。真实队员扫码授权后的公网上传仍待验收。

交互终端运行导出或删除时，须显式设置 `TELEMETRY_DB`。systemd 环境变量不自动进入终端。

接收端在启动时执行一次保留期清理与汇总，此后每 24 小时执行。生产汇总表已有数据；清理效果、错误告警与长期运行仍需运维验证。

遥测 SQLite 库使用独立的每日一致性备份，`cst-telemetry-backup.timer` 已启用，CSTOA 使用 Python 3.11。备份保存在 `/var/lib/cst-telemetry/backups`，保留 30 天。异机保存和完整恢复验证待完成。

`GET /healthz` 返回 `ok`、会话条数、提交版本 `version` 与构建时间 `builtAt`。磁盘容量、定时任务和备份成功状态需额外监控，日志走 journald。

## Tim 接收端

`timserver_1` 已部署同款 Go 二进制，独立 Docker Compose 项目 `cst-pilot-telemetry`。源码与部署文件位于独立仓库 `cst-pilot-server/deploy/timserver_1/`。

| 项 | 实际值 |
|---|---|
| 服务器 | `8.163.28.9` |
| 部署目录 | `/srv/cst-pilot-server` |
| 公网上传 | `https://8.163.28.9:8445/api/telemetry` |
| 内部监听 | Docker 网络 `receiver:8787`，不映射宿主端口 |
| 数据库 | `/srv/cst-pilot-server/data/telemetry.db` |
| 身份 | 真实 OA HTTPS 内省；服务凭据仅保存在服务器 |
| HTTPS | 独立 Caddy 网关；专用公开 CA 随扩展发行，CA 私钥不外发 |
| 证书续期 | `cst-telemetry-tls.timer` 每日检查，证书剩余不足 30 天时续签并重启本项目网关 |
| 备份 | `cst-telemetry-backup.timer` 每日在线一致性备份，保留 30 天 |
| 安全组 | 仅新增 TCP 8445 入站规则，不改现有服务端口 |

两端独立存储，不互相复制数据库。客户端发送相同记录，各端分别确认、去重。

## 运维边界

- 两端只提供上传与健康检查，不提供对外读接口。报表和按成员、设备删除数据通过命令行完成。
- 部署与备份代码位于独立仓库 `cst-pilot-server`，不随工具包发行。
- 报错原文的保留期同时约束数据库与备份，规则见[接收端规格](receiver/SPEC.md#保留期)。
- 真实队员双端入库、异机备份、完整恢复和监控责任仍待验收，见[待处理议题](../issues.md#遥测运维)。

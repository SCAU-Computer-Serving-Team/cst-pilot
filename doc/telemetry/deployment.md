# 遥测部署

同款 Go 接收端分别部署在 CSTOA 与 Tim 的 `timserver_1`，数据库和确认状态独立。接口与存储规则见[接收端规格](receiver/SPEC.md)，验证方法见[遥测测试](../test/telemetry.md)。

## 当前部署

2026-10-27，两个接收端已更新到同一构建包，支持 v0.1、v0.2、会话快照、修订确认和心跳。客户端源码已合并，便携包发布与 USB 实机验收待执行。

| 项 | 已验证结果 |
|---|---|
| 接收端提交 | `c56eb44da8e92ffd325cf20e834c8dfb3bc31bb2` |
| 构建时间 | `2026-10-27T03:22:53Z` |
| 二进制 SHA-256 | `39c3aa553178f4486cbbd0b8c793ed506d6da8c1c10e4c26fc937f8a6729a7c9` |
| CSTOA | systemd 服务正常，原有 22 条会话及原始 payload 保留 |
| Tim | Compose 接收端健康，64 MiB 限制保留，原有 21 条会话及原始 payload 保留 |
| 公网 | 两端 HTTPS 校验通过，能力声明正确，无效凭据返回 401 |
| 数据与备份 | 两端数据库及更新后备份均通过 SQLite `quick_check` |
| 凭据与 CA | 两端凭据文件哈希未变，Tim 专用 CA 哈希未变 |
| 定时任务 | 两端备份定时任务、Tim 证书续期定时任务已启用且运行中 |
| 迁移内存 | 无网络的独立 Linux 64 MiB 容器完成 96 MiB 历史 payload 迁移与启动，未触发内存强杀；健康时占用约 16 MiB |

回退文件分别位于 `/opt/cst-pilot-server/releases/c56eb44da8e9-previous` 与 `/srv/cst-pilot-server/releases/c56eb44da8e9-previous`。此次更新仅操作接收端及其备份、证书定时任务，OA 与 nginx 保持运行。

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
| 内存 | Go 内存软限制 24 MiB；systemd 未设置硬限制 |

部署验收分别核对首次设备授权、已有凭据续期和双端上传，方法见[遥测测试](../test/telemetry.md#真实服务与环境验证)。

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

## CSTOA 扣点能力发布

接收端的扣点字段、迁移、汇总和 CSV 支持已部署。OA #133 仍待合并与部署，客户端便携包尚待发布；真实账单验收待接口就绪后执行。

1. OA：单独确认发布只读账单接口及 `x-oneapi-request-id` 透传，验证成员归属与撤销。
2. 客户端：发布请求观测与账单查询。OA 未就绪或查询失败时保留未知；旧客户端记录缺失新字段仍可接收。

## 触发与心跳改造发布

两个接收端已完成备份、更新及协议能力确认。累计快照、心跳、关闭恢复、页面提示与本地诊断的客户端源码已合并，隔离测试与真实 Pi 到双本地接收端联调通过。客户端便携包发布及实机验收见[改造计划](trigger-upgrade-plan.md#发布与回退)。

心跳与统计复用现有 HTTPS 入口，不新增公网端口或公开读取接口。上线核对心跳超时、快照确认版本、待发数量、汇总计数与备份兼容；一端未就绪时不得误确认该端新快照。

回退先停止新增协议流量，再核对客户端队列与数据库兼容。部署脚本恢复旧程序与配置，保留当前数据库、待发队列及新增数据。数据库恢复须另行确认。

## 运维边界

- 两端只提供上传与健康检查，不提供对外读接口。报表和按成员、设备删除数据通过命令行完成。
- 部署与备份代码位于独立仓库 `cst-pilot-server`，不随工具包发行。
- 报错原文的保留期同时约束数据库与备份，规则见[接收端规格](receiver/SPEC.md#保留期)。
- 真实已有账号双端入库已验证；多队员、异机备份、完整恢复和监控责任仍待验收，见[待处理议题](../issues.md#遥测运维)。

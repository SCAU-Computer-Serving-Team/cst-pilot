# 遥测部署与运维

本页记录 CSTOA 与 `timserver_1` 两个遥测接收端的部署配置与备份规则，并汇总命令行运维操作与升级发布顺序。服务端源码位于独立仓库 `cst-pilot-server`。

## 环境变量与双端部署

接收端通过环境变量配置：

```ini
TELEMETRY_PORT=8787
TELEMETRY_HOST=127.0.0.1
TELEMETRY_DB=/var/lib/cst-telemetry/telemetry.db
OA_INTROSPECT_URL=http://127.0.0.1:8080/api/oauth/introspect
OA_SERVICE_TOKEN=<仅保存在服务器的内省服务凭据>
```

| 项目 | CSTOA 接收端 | Tim 接收端（`timserver_1`） |
|---|---|---|
| 部署方式 | systemd `cst-telemetry.service`（用户 `csttele`，`ProtectSystem=strict`） | Docker Compose `cst-pilot-telemetry`（目录 `/srv/cst-pilot-server`） |
| 公网入口 | `https://www.cstoa.top/api/telemetry`（固定带 `www`，避免裸域 301 重定向丢失 POST） | `https://8.163.28.9:8445/api/telemetry`（安全组仅开放 TCP 8445） |
| 反向代理与 TLS | nginx `location = /api/telemetry` 转发 `127.0.0.1:8787/v1/sessions` | 独立 Caddy 网关配专用公开 CA（`cst-telemetry-tls.timer` 剩余少于 30 天自动续签） |
| 数据库与备份 | `/var/lib/cst-telemetry/telemetry.db`，备份目录 `/var/lib/cst-telemetry/backups/` | `/srv/cst-pilot-server/data/telemetry.db`，备份目录 `/srv/cst-pilot-server/backups/` |
| OA 内省地址 | 本机回环 `http://127.0.0.1:8080/api/oauth/introspect` | 公网 HTTPS `https://www.cstoa.top/api/oauth/introspect` |

两端独立存储、独立验签并独立确认，不共享或同步数据库。

两个接收端已部署提交 `c56eb44da8e92ffd325cf20e834c8dfb3bc31bb2`（构建时间 `2026-10-27T03:22:53Z`，二进制 SHA-256 为 `39c3aa553178f4486cbbd0b8c793ed506d6da8c1c10e4c26fc937f8a6729a7c9`），支持 `0.1` 与 `0.2` 协议、会话快照、修订确认及心跳。回退二进制分别保留在 CSTOA 的 `/opt/cst-pilot-server/releases/c56eb44da8e9-previous` 与 Tim 的 `/srv/cst-pilot-server/releases/c56eb44da8e9-previous`。

## 备份、保留期清理与命令行运维

1. **自动清理与汇总**：启动时及每 24 小时清理满 180 天的 `sessions.payload.errors` 报错原文、删除满 90 天的 `uploads` 日志并重算 `daily_rollup`。启动时及每 15 秒扫描 `process_runs`，将超 90 秒未更新的记录标为失联并删除满 90 天的历史运行记录。
2. **每日备份**：两端启用 `cst-telemetry-backup.timer` 每日执行 SQLite 在线一致性备份，保留 30 天。
3. **命令行操作**（在服务器终端执行离线子命令时须显式传入 `TELEMETRY_DB`）：

| 子命令 | 用途与规则 |
|---|---|
| `telemetry-receiver [serve]` | 默认模式，启动 HTTP 接收服务与后台定时清理任务 |
| `telemetry-receiver version` | 输出二进制提交哈希与构建时间 |
| `telemetry-receiver export-csv [out]` | 导出会话派生列明细 CSV（默认写出 `./telemetry-sessions.csv`，含身份、环境、模型用量与 `cstoa_*` 扣点列，不含报错正文） |
| `telemetry-receiver rollup` | 清空派生汇总并从 `sessions` 原始记录全量重算 `daily_rollup` |
| `telemetry-receiver delete --mid <id> \| --device <id> \| --installation <id>` | 按上传成员、OA 设备或匿名安装删除会话记录并重算汇总。按安装删除时保留 `installations` 表中的公钥绑定，防止其他密钥冒用同一安装 ID |

## 客户端投递诊断

`agent/home/telemetry/status.json` 记录本地最近保存的快照、各端入队确认与心跳确认，以及最近失败类别（`storage`、`auth`、`capability`、`protocol`、`network`、`timeout`、`tls`、`http`）。端点按 URL 的 SHA-256 前 16 位标识，不记录会话正文、凭据或原始端点地址。诊断写入失败不阻塞业务或队列清理。

## 发布与回退

双端接收端已完成快照与心跳能力部署。OA 请求级账单接口与新版客户端便携包上线待推进。

涉及协议或字段扩展时，按以下顺序上线：

1. **OA 侧先行**：发布 `/api/agent/llm/billing` 只读账单接口与推理响应头 `x-oneapi-request-id` 透传，核验成员与 APIKEY 归属校验。
2. **双端接收端升级**：先执行双端 SQLite 备份，再部署新版 `telemetry-receiver`，通过 `GET /api/telemetry` 与 `GET /healthz`（CSTOA 本机 `127.0.0.1:8787/healthz`，Tim 公网 `8445/healthz`）核验能力声明及 `0.1` / `0.2` 协议兼容性。
3. **客户端发布**：在接收端能力就绪后发布新版工具包。某一接收端尚未升级时，客户端能力检查会将 `0.2` 快照保留在本地队列中，不误删或降级丢弃新字段。
4. **回退保护**：回退前先停止新协议流量并备份当前数据库，保留客户端待发队列与服务端已有 `sessions` 记录。

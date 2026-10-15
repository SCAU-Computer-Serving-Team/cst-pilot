# 接收端

接收端校验上传凭据、保存会话统计，提供命令行导出和删除。协议版本 `0.1`，字段见[信息收集契约](../../contract.md)和[会话记录](../schema.md)，部署见[部署说明](../deployment.md)。

## 接口

| 接口 | 行为 |
|---|---|
| `POST /v1/sessions` | 上传 1–50 条会话记录，完整正文最多 1 MiB |
| `GET /healthz` | 返回 ok、会话条数、version、builtAt；数据库不可读时失败 |

公网入口将 /api/telemetry 转发到 /v1/sessions，不提供读记录或下发指令的接口。

| 响应 | 发送端处理 |
|---|---|
| 202 `{ accepted, rejected, reasons }` | 整批确认，逐条拒收也不重发 |
| 401 | 凭据或签名无效，保留记录 |
| 403 | OA 吊销、改密，或安装已绑定其他公钥，保留记录 |
| 400 / 413 | 正文、条数或体积不合法，丢弃该批 |
| 429 | 限流，保留并重试；不写该请求的上传日志 |
| 5xx | 服务或身份服务故障，保留并重试 |

## 身份解析

### OA 凭据

Bearer 访问令牌由接收端向 OA 内省，取 mid 和 device_id。服务凭据仅放服务器，接口不跟随重定向；OA 服务不可用返回 503，不缓存故障。

| 内省结果 | 处理 |
|---|---|
| active=true 且包含 mid、device_id | 写入上传者 mid、deviceId |
| expired / invalid | 401 |
| revoked / password-changed | 403 |
| 非 200、解析失败或超时 | 503 |

正常结果按令牌 SHA-256 缓存 60 秒，仅保存在内存，不写令牌或哈希日志。不额外校验模型调用作用域。

### 安装签名

未登录使用 Installation 鉴权及 Ed25519 签名，格式见[发送端](../sender/SPEC.md#凭据与配置)。首次有效签名自动绑定公钥；后续必须保持相同公钥，服务重启不解除绑定。

1. 签名覆盖安装标识、时间和正文摘要，时间允许前后 10 分钟；先验签，再注册安装。
2. 每个安装每分钟最多 60 次、每个连接来源每分钟最多 120 次；内存计数有界。通过反向代理时同一连接来源限额由该入口共享，不能把它当真实用户 IP 限流。
3. 记录声明的非空 installationId 必须与安装鉴权一致，否则逐条拒收。无新增身份的记录允许兼容接收。
4. 安装签名只证明持有密钥，无法阻止创建多个新安装。自然人认证、恶意批量注册告警与密钥遗失处理需独立管理。

### 归属边界

mid、deviceId 在数据库列和 payload 中均由本次上传凭据确定，匿名上传的 payload 为 null。客户端提供的同名字段覆盖清除；这两个字段只表示上传者，不据此确定排队会话的操作人。

identity 和环境保持客户端会话快照；相同 recordId 重传保留首次记录，不能通过重传补改归属、环境或接收时间。匿名安装、系统环境和上传账号分别统计。

## 校验

整批拒绝：鉴权失败、无效 JSON、records 非数组或为空、超过 50 条或正文超过 1 MiB。

| 逐条检查 | 拒收原因 |
|---|---|
| v 属于支持集合，目前为 0.1 | unknown-version |
| type=session | unknown-type |
| recordId 非空字符串且最多 64 字符 | bad-record-id |
| tools≤512、models≤32、errors≤10 | too-many-elements |
| 安装鉴权与记录非空安装标识不匹配 | installation-mismatch |

数字类型不符按 0，缺少数组按空数组，缺少新增环境字段保持未知。未知字段原样保存在 payload，不以业务合理性检查拒收。

receivedAt 使用服务端时钟；ip 来自连接，IPv4 存 /24 网段。经过反向代理时记录连接来源，不能作为会话地点或用户识别依据。

## 存储

使用 Go、纯 Go SQLite、单数据库文件和 WAL。原始 payload 与派生列在同一事务中保存，record_id 为去重主键；新增字段迁移只增加派生列与索引，不改已有原始记录。

| 表 | 内容 |
|---|---|
| sessions | 会话正文、上传者、接收时间、网段、统计、费用及环境派生列 |
| installations | 安装 ID、绑定公钥、首次注册时间；不保存私钥或原始 MachineGuid |
| devices | 有 OA 登录标识的最近接入信息，匿名上传不创建空设备行 |
| members | 编号到姓名与队伍的展示资料，独立于鉴权，不强制外键 |
| daily_rollup | 按接收时间 +08:00 日期、上传者 mid、installation_id、environment_id 聚合 |
| uploads | 请求结果、条数、拒收原因、插入数、状态与耗时，不含正文、令牌、签名或密钥 |

sessions 新增派生列：installation_id、environment_id、environment_source、os_platform、os_name、os_display_version、os_is_pe、runtime_version、runtime_arch。未知值为空；已有 os_version、os_arch、admin 继续保存，payload 区分 null 和缺失。

索引覆盖接收时间与上传者、OA 登录标识、会话 ID、工具包版本、安装及环境。不同币种分别汇总，未定价单列；峰值与窗口成对保存。缺失标识只参与未知组统计，不计为一个已识别用户。

汇总不保存报错原文。结构变化时可清空派生汇总并从 sessions 重建，原始会话保留。

## 保留期

| 内容 | 保留与清理 |
|---|---|
| 会话统计、身份、公钥与汇总 | 长期保存 |
| sessions.payload.errors | 180 天后移除 |
| uploads | 90 天后删除 |

启动及每 24 小时清理、重算汇总。模型报错原文可能含路径或账号，备份同样遵循原文保留边界。

## 运维接口

| 命令 | 用途 |
|---|---|
| export-csv | 导出会话派生列，包含新身份、系统名称和运行时字段，不导出报错正文 |
| rollup | 重算每日汇总 |
| delete --mid / --device / --installation | 按上传账号、OA 登录标识或匿名安装删除会话，再重算汇总 |

命令行操作显式指定 TELEMETRY_DB。按安装删除会话仍保留公钥绑定，避免其他密钥重新占用同一标识；安装密钥恢复需独立运维流程。部署不开放远程删除接口，验证状态见[遥测验证](../../test/telemetry-report.md)。

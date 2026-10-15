# 遥测验证

验证日期：2026-10-15。环境：Windows 11 Pro（10.0.26200 / 25H2）、Node 24.15.0、本机 Go 接收端、CSTOA 与阿里云 `timserver_1`。

## 结论

发送端已按新契约采集安装、环境和运行时字段。未登录 CSTOA 时可用安装签名自动上传；真实 Pi 会话已通过公网写入两个生产接收端，完整 payload 与本地记录逐字段一致，两端队列均已确认清空。

两端部署前已备份，已有记录、OA 服务凭据与 Tim CA 保留。新增生产测试记录使用 `kitVersion=telemetry-e2e-20261015`，与真实使用统计分开识别。

## 自动验证

| 范围 | 结果 |
|---|---|
| 发送端 | 20 项通过：安装与环境、PE／探测失败、凭据、队列、草稿、双端独立确认及 TLS |
| 跨进程持久化 | 四个独立 Node 进程各追加 25 条，返回一致安装 ID，100 条记录全部保留且 ID 唯一 |
| 草稿故障恢复 | 注入入队失败后源草稿保留；后续恢复保留 recordId、原系统环境与已定稿时间 |
| Go 接收端 | 12 个顶层测试通过：签名篡改、错误密钥、服务重建后绑定保留、限流、OA、去重、CSV、保留期及数据库迁移 |
| 数据迁移 | 按已有结构建立旧库，升级后原会话仍可导出；新增运行时未知值不伪造；两个匿名安装分开汇总 |
| 跨仓库端到端 | 真实 Pi 自动发现扩展，模型与 OA 使用本机替身，两个真实 Go 接收端入库 |
| 自动补发 | 停止并重启一个接收端，不再派发会话事件；30 秒定时重试清空该端队列。OA 凭据被拒后使用安装签名，上传者为空 |
| 归属与环境 | 环境 A 的待发样例在后续实例补发仍保留原环境；匿名、OA 上传与重复记录分别核对 |
| 工程检查 | 后端 Biome、TypeScript、Go vet、2 项备份测试及 61 项源码／发行检查通过 |
| 相关回归 | Web 后端 95 项、OAuth 11 项通过 |

本机端到端入口：

```powershell
$env:CST_TELEMETRY_RECEIVER = '接收端可执行文件的绝对路径'
npm run test:telemetry:e2e --prefix agent
```

测试位于 `agent/home/extensions/telemetry/test/receiver.e2e.ts`。未配置二进制时跳过，常规 CI 不自动执行跨仓库或生产上传。

## 生产部署与入库

两端使用同一 Linux 二进制，版本为源码快照标识 `b4b63416e305793a2b327b1adf3ed96d7f89ad45`。该值用于核对此次源码快照，与 Git 提交号分别记录；候选包保存 source.tar.gz、SOURCE-MANIFEST.json 与 BUILD-INFO.json，后者标记 sourceKind=worktree-snapshot。

二进制 SHA-256：`e448b09291e43e9b45a5dee4fb5d00dac64923cc7e3b151d56d0418a34d8da7b`。构建时间为 `2026-10-15T06:47:09Z`。

| 检查 | CSTOA | Tim / aliyun |
|---|---|---|
| 更新方式 | 在线备份、替换接收端、重启独立 systemd 服务 | 在线备份、更新独立遥测 Compose 项目 |
| 健康与版本 | ok，版本匹配 | receiver healthy，版本匹配 |
| 原记录保留 | 更新前 13 条全部保留 | 更新前 12 条全部保留 |
| 本轮新增记录 | 1 条，合计 14 条 | 相同记录 1 条，合计 13 条 |
| 重传 | 返回 accepted，同一 recordId 仍为 1 条 | 相同 |
| 无效 OA 令牌 | 拒绝，无新增会话 | 相同 |
| 匿名字段 | payload.mid、deviceId 均为 null | 相同 |
| 数据完整性 | quick_check=ok | quick_check=ok |
| 安装公钥 | 首次有效签名已绑定 | 独立绑定相同安装 |
| 配置与 CA | 凭据哈希未变 | 凭据及客户端 CA 哈希未变 |

生产测试从隔离 home 启动真实 Pi，使用本机模型替身生成一轮回复。OA 未登录，发行遥测配置直连两个公网 HTTPS 入口；第三个不可达测试端点仅用于保留同一记录作本地对照，不影响生产端确认。

| 字段 | 实际入库值 |
|---|---|
| recordId | `8291bc2f-b014-4e48-8366-c2810cf868bb` |
| installationId | `f5d0b9d3-ea0b-457b-a56b-acf292726267` |
| environmentId | `6c5a666a4a02925255f015a999b1ad0e48fdbe72d605aca60581c5f7c2dda087` |
| os.name / displayVersion | Microsoft Windows 11 Pro / 25H2 |
| os.platform / version / arch / isPE | win32 / 10.0.26200 / x64 / false |
| runtime.version / arch | 24.15.0 / x64 |
| admin | false，派生列为 0 |
| prompts / turns / channel | 1 / 1 / web |

从两端只读查询该测试记录，移除服务端补充字段后与本地 JSON 完全一致；新增派生列与原始 payload 一致。未复制开发目录凭据，未向模型或服务器发送 MachineGuid 原文、私钥、账户 SID 或诊断内容。

## 可靠性缺口

| 范围 | 尚待验证或限制 |
|---|---|
| 完整发行副本 | 本轮使用隔离 home 与真实扩展，未重建完整便携包，第三方扩展包未加载 |
| 实际 U 盘 | FAT/exFAT、写保护、跨物理电脑、拔盘与磁盘暂不可写组合仍需实机覆盖 |
| 系统与权限 | 实测环境为 Windows 11 x64 普通权限；Windows 10、ARM64、真实 PE、跨账户 UAC 待补 |
| 真实 OA 用户 | 本机验证使用 OA 替身；生产完成匿名上传，真实队员扫码及多队员上传仍需验收 |
| 退出与容量 | 正常退出不等待网络；强杀只恢复已写草稿，队列最多 200 条，超限淘汰最旧记录 |
| 运行时长 | 已验证 30 秒补发，长期网络抖动、多进程与磁盘故障组合待持续运行验证 |
| 安装鉴权 | 初次公钥绑定仅证明密钥持有权；批量新安装滥用告警、密钥遗失处理仍需运维方案。时钟偏差超过 10 分钟不能通过签名验证 |
| 备份与告警 | 两端同机一致性备份可用，异机备份、完整恢复和上传时效／容量／证书告警仍待验收 |

CSTOA 公网 /healthz 返回前端 HTML，接收端监控使用内部 `127.0.0.1:8787/healthz` 并校验内容。Tim 使用专用 CA 验证公网健康接口。

## 证据位置

本机证据统一保存在 `E:/tmp/2026-10-15/`：

- `new-telemetry-unit.log`、`new-telemetry-e2e-final.log`、`new-agent-check-final.log`。
- `new-receiver-tests.log`、`new-receiver-backup-tests.log`、`new-contract-pack.log`。
- `deploy-timserver-new.log`、`deploy-cstoa-new.log`、`receiver-snapshot/`。
- `live-telemetry.log`、`live-telemetry-result.json`、`verify-live-tim.json`、`verify-live-cstoa.json`。
- `live-telemetry-v5MItv/record.json` 保存同一会话的本地对照，测试安装密钥仅留在该隔离目录，不进入证据正文或仓库。

两端候选目录为 `/root/cst-telemetry-20261015-b4b63416e305/`；回退目录分别为部署根下 `releases/b4b63416e305-previous/`。常规提交版本发布与回退规则见独立接收端仓库的 doc/release.md。

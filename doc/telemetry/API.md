# 遥测协议与接口

本文定义客户端遥测配置、Rust 宿主接口、上传签名算法与接收端 HTTP 协议。字段语义见 [../contract.md](../contract.md) 与 [SPEC.md](SPEC.md)。

## 客户端配置

`agent/home/telemetry.json` 在服务启动时读取：

```json
{
  "enabled": true,
  "endpoints": [
    { "url": "https://www.cstoa.top/api/telemetry" },
    { "url": "https://8.163.28.9:8445/api/telemetry", "caFile": "timserver_1.crt" }
  ],
  "authProvider": "cstoa"
}
```

公网端点必须使用 HTTPS（本机回环测试允许 HTTP），拒绝包含用户名、密码或片段的 URL。`caFile` 限定 `[A-Za-z0-9_-]+\.crt` 文件名，固定在 `agent/home/extensions/telemetry/<caFile>` 下读取，仅用于对应端点的证书链校验并保留主机名核验（文件缺失或无效仅将该端点标为不可用）。同目录 `agent/home/extensions/telemetry/currency.json` 提供服务商币种映射（`CNY` / `USD`）。

## Rust 宿主接口（`TelemetryService`）

可信宿主通过 `KitHost` 或 `Kit::create_session` 自动绑定遥测生命周期。低层 `Kit::session_options` 仅装配工具与资源，不自动启动遥测。

| 接口 | 调用边界 |
|---|---|
| `KitSession / HostedSession::telemetry()` | 返回当前工具包 home 共享的 `TelemetryService`，禁用时返回 `None` |
| `KitSession / HostedSession::finalize_telemetry(reason)` | 删除或释放会话前显式定稿，在当前关闭预算内完成本地落盘与入队，不等待网络上传 |
| `TelemetryService::hint()` | 页面切出、恢复可见或网络恢复时触发提交与补发，2 秒内重复调用自动合并 |
| `TelemetryService::begin_close()` | 进程真实退出前建立同 home 共享的 5 秒截止时刻，后续会话关闭共用该预算 |
| `TelemetryService::cancel_close()` | 宿主取消退出流程时清除当前截止时刻 |
| `TelemetryService::close_process(reason)` | 并行定稿全部活跃实例，在共享截止时刻内返回，停止定时调度并触发一次 `closed` 心跳 |

## 上传鉴权与安装签名

| 请求头 | 格式与规则 |
|---|---|
| `Authorization` | `Bearer <OA access_token>` 或 `Installation <installationId>` |
| `X-Telemetry-Installation` | 客户端签名时固定携带，服务端在 `Bearer` 鉴权下由此读取 `installationId`（`0.2` 协议必填） |
| `X-Telemetry-Key` | Ed25519 公钥的 DER/SPKI 字节，base64url 编码（无 `=` 填充） |
| `X-Telemetry-Time` | 当前 Unix 毫秒时间戳字符串，服务端允许 `±10` 分钟偏差 |
| `X-Telemetry-Signature` | Ed25519 签名的 base64url 编码（无填充） |

签名密钥派生与原文构造：

1. **私钥种子**：`seed = HMAC-SHA256(identityKey, UTF-8("upload-signing"))`（32 字节，与 `environmentId` 派生域隔离）。
2. **签名原文**：以下 4 行使用 `\n` 连接，末尾无换行：
   ```text
   telemetry-upload
   <installationId>
   <X-Telemetry-Time 原文>
   <请求体原始 UTF-8 字节的 SHA-256 小写十六进制>
   ```

## 接收端与账单接口

### 1. 能力声明与健康检查

- `GET /v1/sessions`（公网经 `/api/telemetry` 转发）：返回服务端支持的协议与能力列表，客户端确认包含 `"0.2"` 与三项能力后启用版本化快照及心跳：
  ```json
  { "protocols": ["0.1", "0.2"], "capabilities": ["session-snapshots", "revision-acks", "heartbeat"] }
  ```
- `GET /healthz`：返回 `{ "ok": true, "sessions": 128, "version": "...", "builtAt": "..." }`。

### 2. 上传与心跳：`POST /v1/sessions`

- **请求限制**：单批 `records` 最多 50 条，请求体最大 1 MiB。`0.2` 携带 `heartbeat` 时允许 `records: []`。
- **`0.2` 响应（HTTP `202`）**：
  ```json
  {
    "v": "0.2",
    "accepted": 1,
    "rejected": 0,
    "reasons": {},
    "acks": [{ "recordId": "31de2e2e-...", "revision": 2, "status": "stored" }],
    "heartbeatAck": { "runId": "9b1deb4d-...", "sequence": 4, "status": "stored" }
  }
  ```
  - `acks[].status` 为 `"stored"`、`"duplicate"` 或 `"stale"` 时均表示服务端已确认，客户端核对队列当前记录仍与发送内容一致后移除该项。
  - `acks[].status` 为 `"rejected"` 且 `reason` 属于确定性协议或归属错误（`bad-revision`、`bad-final-state`、`bad-record-id`、`unknown-type`、`too-many-elements`、`installation-mismatch`、`immutable-legacy-record`、`owner-mismatch`、`instance-mismatch`、`already-final`、`envelope-version-mismatch`）时，移除对应记录并登记拒收状态。其他原因保留待查。
- **HTTP 状态码处理**：
  - `401` / `403`：当前使用 OA `Bearer` 时自动回退为 `Installation` 签名重发，保留队列记录。
  - `429` / `5xx` / 网络超时（单次 5 秒）：保留队列，等待下次触发或 30 秒调度重试。
  - `400` / `413`：`0.2` 快照保留队列。`0.1` 协议批次从对应队列移除。
  - `404` / `410`：当前进程停止请求该端点，保留磁盘队列。

### 3. CSTOA 请求账单查询：`POST /api/agent/llm/billing`

- **请求**：携带有效 CSTOA OAuth `access_token` 或 `APIKEY`，正文 `{ "requestIds": ["..."] }`（每批最多 50 个，单次超时 2 秒，总预算不超过 4.5 秒，禁止重定向）。
- **响应**：返回归属当前成员的已确认扣点明细，客户端汇总为 `cstoaBilling: { requests, settledRequests, quotaPoints }`。

## 会话快照 JSON 示例

```json
{
  "v": "0.2",
  "type": "session",
  "recordId": "31de2e2e-816e-4cdc-8f35-c8eae6e69272",
  "revision": 2,
  "final": true,
  "kitVersion": "v0.6.2",
  "sessionId": "69194984-ceb2-4abd-849c-90c85da1051b",
  "identity": {
    "installationId": "df79039e-3744-44c8-9754-7a9568ae5684",
    "environmentId": "7b28539aed99a4a8b9c422d017d9838e8c9c43d7b0ab986b4988b0808f174c12",
    "environmentSource": "windows-machine-guid"
  },
  "os": { "platform": "win32", "name": "Windows 11 Pro", "version": "10.0.26100", "displayVersion": "24H2", "arch": "x64", "isPE": false },
  "runtime": { "version": "0.1.0", "arch": "x64" },
  "admin": true,
  "channel": "web",
  "reason": "new",
  "endReason": "quit",
  "startedAt": "2026-10-15T09:00:00+08:00",
  "endedAt": "2026-10-15T09:00:10+08:00",
  "durationMs": 10000,
  "activeMs": 4800,
  "prompts": 1,
  "turns": 1,
  "contextEntries": 2,
  "models": [{
    "provider": "cstoa",
    "model": "deepseek-v4.1-flash",
    "thinkingLevel": "high",
    "turns": 1,
    "input": 500,
    "output": 120,
    "cacheRead": 0,
    "cacheWrite": 0,
    "totalTokens": 620,
    "cost": null,
    "currency": "CNY",
    "incompleteUsageTurns": 0
  }],
  "cstoaBilling": { "requests": 1, "settledRequests": 1, "quotaPoints": 98 },
  "compactions": 0,
  "compactionTokens": 0,
  "compactionOverflows": 0,
  "compactionFailures": 0,
  "contextPeak": 620,
  "contextWindow": 200000,
  "tools": [],
  "providerErrors": {},
  "networkErrors": 0,
  "aborted": 0,
  "toolFailures": 0,
  "errors": []
}
```

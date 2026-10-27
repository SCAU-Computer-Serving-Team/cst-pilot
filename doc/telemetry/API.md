# 遥测协议与接口

本文定义客户端遥测配置、上传鉴权与 Ed25519 签名算法、接收端 HTTP 接口及完整会话快照示例。字段语义见 [../contract.md](../contract.md) 与 [SPEC.md](SPEC.md)。

## 客户端配置

`agent/home/telemetry.json`（启动时加载）：

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

公网端点必须使用 HTTPS（本机回环测试允许 HTTP），拒绝包含用户名、密码或片段的 URL。`caFile` 指向扩展目录下的专用公开 CA 证书，仅用于对应端点的 TLS 链校验。

## 上传鉴权与安装签名

| 请求头 | 格式与规则 |
|---|---|
| `Authorization` | `Bearer <OA access_token>` 或 `Installation <installationId>` |
| `X-Telemetry-Installation` | `0.2` 协议使用 `Bearer` 鉴权时必填，指明当前签名的 `installationId` |
| `X-Telemetry-Key` | Ed25519 公钥的 DER/SPKI 字节，base64url 编码（无 `=` 填充） |
| `X-Telemetry-Time` | 当前 Unix 毫秒时间戳字符串（服务端允许 `±10` 分钟偏差） |
| `X-Telemetry-Signature` | Ed25519 签名的 base64url 编码（无填充） |

签名密钥派生与原文构造：

1. **私钥种子**：`seed = HMAC-SHA256(identityKey, UTF-8("upload-signing"))`（32 字节，与 `environmentId` 派生域分离）。
2. **签名原文**：以下 4 行用 `\n` 连接（末尾无换行）：
   ```text
   telemetry-upload
   <installationId>
   <X-Telemetry-Time 原文>
   <请求体原始 UTF-8 字节的 SHA-256 小写十六进制>
   ```

## 接收端与账单接口

### 1. 能力声明与健康检查

- `GET /v1/sessions`（公网经 `/api/telemetry` 转发）：返回支持协议与能力声明，客户端逐端确认包含 `"0.2"` 与所需能力后启用版本化快照与心跳：
  ```json
  { "protocols": ["0.1", "0.2"], "capabilities": ["session-snapshots", "revision-acks", "heartbeat"] }
  ```
- `GET /healthz`：返回 `{ "ok": true, "sessions": 128, "version": "...", "builtAt": "..." }`。

### 2. 上传与心跳：`POST /v1/sessions`

- **请求限制**：单批 `records` 最多 50 条，请求体最大 1 MiB；`0.2` 含 `heartbeat` 时允许 `records: []`。
- **`0.2` 响应（HTTP `202`）**：
  ```json
  {
    "v": "0.2",
    "accepted": 1,
    "rejected": 0,
    "acks": [{ "recordId": "31de2e2e-...", "revision": 2, "status": "stored" }],
    "heartbeatAck": { "runId": "9b1deb4d-...", "sequence": 4, "status": "stored" }
  }
  ```
  - `acks[].status`：`"stored"`（新写入或替换低版本）、`"duplicate"`（同版本重传）、`"stale"`（服务端已有更高版本或已 `final`）均视为服务端已确认，客户端可从队列移除该版本；`"rejected"` 附 `reason`。
  - **逐条拒收码（`reason`）**：`unknown-version`、`unsupported`（这两项客户端保留队列待服务端升级）、`bad-revision`、`bad-final-state`、`unknown-type`、`bad-record-id`、`too-many-elements`（`tools > 512` / `models > 32` / `errors > 10`）、`installation-mismatch`、`ownership-mismatch`。
- **HTTP 错误码处理**：
  - `401` / `403`：凭据或签名被拒；若当前使用 OA `Bearer`，客户端回退尝试 `Installation` 签名，记录保留在队列中。
  - `429` / `5xx` / 网络超时（单次 5 秒）：保留队列，等待下次触发或 30 秒重试。
  - `404` / `410`：当前进程停止向该端点发送，保留磁盘队列。

### 3. CSTOA 请求账单查询：`POST /api/agent/llm/billing`

- **请求**：携带 CSTOA OAuth `access_token` 或 `APIKEY`，正文 `{ "requestIds": ["..."] }`（最多 50 个，超时 2 秒，禁止重定向）。
- **响应**：返回已归属当前成员（及当前 APIKEY）的确认扣点明细；客户端汇总为 `cstoaBilling: { requests, settledRequests, quotaPoints }`。

## 会话快照 JSON 示例

```json
{
  "v": "0.2",
  "type": "session",
  "recordId": "31de2e2e-816e-4cdc-8f35-c8eae6e69272",
  "revision": 2,
  "final": true,
  "kitVersion": "v0.5",
  "sessionId": "69194984-ceb2-4abd-849c-90c85da1051b",
  "identity": {
    "installationId": "df79039e-3744-44c8-9754-7a9568ae5684",
    "environmentId": "7b28539aed99a4a8b9c422d017d9838e8c9c43d7b0ab986b4988b0808f174c12",
    "environmentSource": "windows-machine-guid"
  },
  "os": { "platform": "win32", "name": "Windows 11 Pro", "version": "10.0.26100", "displayVersion": "24H2", "arch": "x64", "isPE": false },
  "runtime": { "version": "24.15.0", "arch": "x64" },
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

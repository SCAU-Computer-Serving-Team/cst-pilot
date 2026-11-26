---
name: bitlocker
description: bitlocker 卷加密状态查询、48 位恢复密码解锁、完整解密与恢复密钥指引。
---

# bitlocker

实时查询与受控操作 BitLocker 卷，不使用缓存快照。返回结构为 `{ "bitlocker": { ... }, "elapsedMs": N }`。

## 操作步骤与完成判据

### 解锁卷（unlock）

1. 先查 `bitlocker({ scope: "status", drive })` 确认目标卷锁定（`lockStatus: 1`）。已解锁跳过，不索取凭据。
2. 仅在用户明确要求解锁时调用 `bitlocker({ scope: "unlock", drive })`。默认省略 `recoveryPassword`，由本地界面隐藏输入（等待上限 180 秒）。
3. **完成判据**：分开核对 `operation.status` 与实时复查后的 `lockStatus`：
   - `operation.status === "accepted"` 且 `lockStatus === 0`：确认本次解锁成功（不代表关闭加密）。
   - `operation.status === "skipped"` 且 `lockStatus === 0`：报告该卷原已处于解锁状态，不可称本次执行了解锁。
   - `operation.status === "accepted"` 但复查失败（`lockStatus: null`）：结果未确认，立即再调 `status` 核实。

### 完整解密（decrypt）

1. 不将关闭加密作为常规排障手段，仅在用户明确要求解除保护时调用。默认传用户明确指定的 `drive`；仅在用户明确要求解密全部卷时省略 `drive`。
2. 省略 `drive` 时工具评估全部可枚举卷，只将符合条件的卷送入本地确认界面（等待上限 180 秒）；已解密或正在解密的卷直接跳过，锁定或状态不明的卷直接拒绝。
3. 锁定卷被拒绝时，先安排单独的 `unlock` 调用。仅在返回 `error` 或 `returnCode` 明确指向自动解锁密钥依赖、权限不足等原因时如实点明，不自动删保护器或改策略。
4. 批量操作逐卷报告资格、操作状态与复查状态。
5. **完成判据**：请求接受后由 Windows 后台执行，后续用 `status` 跟踪进度；仅 `conversionStatus === 0` 确认完全解密，`encryptionPercentage === 0` 不能单独证明完成。

## 条件分支

- **缺少恢复密码**：
  - 查哪里：指引用户自行打开 <https://aka.ms/myrecoverykey> 登录相关微软账户。
  - 按什么匹配：对照工具返回的恢复密码保护器 `keyProtectors[].id` 匹配 48 位数字恢复密码。账户登录密码、Windows PIN、启动 PIN 均无效，管理员权限不能替代有效凭据。
  - 查不到怎么办：微软账户未备份记录时，检查其他关联账户、单位 IT、已保存文件或打印件。
- **用户已主动在聊天提供恢复密码**：禁止主动索要密码发进聊天。若用户已主动提供并明确要求代填，先告知密码已进入模型上下文且可能发给模型服务商（该告知不消除已发送文本的风险），再决定是否传 `recoveryPassword`；工具仍要求本地确认且不回显密码。

## 参数

| 参数 | 约束与默认值 |
|---|---|
| `scope` | `status`（默认）、`unlock`、`decrypt` |
| `drive` | 单盘符如 `C` 或 `C:`；`unlock` 必填，`status` 与 `decrypt` 省略时评估全部可枚举卷 |
| `recoveryPassword` | 仅 `unlock` 可选，8 组 6 位 ASCII 数字用 `-` 分隔；其他 `scope` 禁止传入 |

## 返回字段与枚举

| 字段 | 结构与说明 |
|---|---|
| 查询级字段 | `scope`、`collectedAt`、`admin`、`source`、`volumes[]`、`notice`，可选 `degraded`、`collectionErrors`、`operation`（异常中断重启后的自动复查；复查失败时可仅返回最小对象与 `degraded: true`） |
| `volumes[]` | `volumeId`、`drive`、`volumeType`、`conversionStatus`、`encryptionPercentage`（%）、`encryptionMethod`、`protectionStatus`、`lockStatus`、`autoUnlockEnabled`（仅数据卷）、`autoUnlockKeysStored`（仅当前系统卷）、`keyProtectors[]: { id, type }`，可选 `collectionErrors`、`operation` |
| `conversionStatus` | `0` 完全解密、`1` 完全加密、`2` 加密中、`3` 解密中、`4` 加密暂停、`5` 解密暂停 |
| `lockStatus` | `0` 已解锁、`1` 已锁定 |
| `operation` | `status`（`accepted` / `rejected` / `skipped` / `cancelled` / `unknown`）、`reason`、`returnCode`（可为 `null`）、`error`（可为 `null`）、可选 `credentialSource`（`localInput` / `toolParameter`，未用凭据时省略） |

## 异常、互斥与限制

1. `operation.status === "unknown"`（超时、调用异常或进程中断重启复查）严禁自动重试，须立即用 `status` 复查真实状态；不承诺已提交的 Windows 操作可通过关闭界面或停止对话撤销。
2. 写操作要求管理员权限与本地交互界面；同卷并发操作会被跨进程锁拒绝。业务拒绝或取消时整次调用仍可能返回 `isError: false`，须逐卷核对 `operation.status`。
3. 仅支持 48 位数值恢复密码解锁，不支持数据卷密码、外部密钥、证书或 `repair-bde`；硬件加密盘结合 `encryptionMethod` 解读。
4. 整次查询失败或指定盘符未找到唯一卷返回 `isError: true`；部分采集错误置 `degraded: true`，未枚举到卷或字段 `null` 均不能判定为未加密。底层单次查询或执行阶段各有 60 秒预算，加上本地交互最长 180 秒等待。

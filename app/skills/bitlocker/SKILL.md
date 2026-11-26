---
name: bitlocker
description: bitlocker 卷加密诊断与受控操作参考。查询卷加密状态（status）、用 48 位恢复密码解锁（unlock）或经本地确认发起完整解密（decrypt）时查阅。
---

# bitlocker

实时查询与受控操作 BitLocker 卷。`status` 只读；`unlock` 解锁访问并保留加密；`decrypt` 解除保护并启动后台完整解密。写操作须经本地界面确认或输入，不绕过人工确认。

## 操作步骤与完成判据

### 解锁卷（unlock）

1. 先用 `status` 确认目标卷已锁定（`lockStatus: 1`）。已解锁卷跳过，不索取凭据。
2. 仅在用户明确要求解锁时调用 `bitlocker({ scope: "unlock", drive })`。默认省略 `recoveryPassword`，由本地界面隐藏输入（180 秒超时）。
3. 用户缺少恢复密码时，指引打开 <https://aka.ms/myrecoverykey> 登录相关微软账户，按 `keyProtectors[].id` 匹配 48 位恢复密码。账户登录密码、Windows PIN、启动 PIN 均无效；管理员权限不能替代有效凭据；有账户不保证已备份，无记录时核查其他相关账户、单位 IT、备份文件或打印件。
4. 用户主动要求 AI 填参时，先告知恢复密码会进入模型上下文并可能发送给模型服务商，再传 `recoveryPassword`。禁止主动让用户把恢复密码发进聊天；工具仍会要求本地确认且不回显密码。
5. **完成判据**：同时核对 `operation` 与 `lockStatus`。仅在 `operation.status === "accepted"` 且实时复查 `lockStatus === 0` 时确认已解锁。解锁成功不等于关闭加密。

### 完整解密（decrypt）

1. 不把关闭加密当作常规排障手段，仅在用户明确要求解除保护时调用。指定 `drive` 仅操作该卷；省略 `drive` 处理全部可枚举卷。
2. 工具在本地界面展示固定目标清单并要求确认（180 秒超时）。无本地交互通道或用户取消时不执行，不尝试绕过确认。
3. 锁定卷会直接拒绝，须先单独执行 `unlock`。系统卷自动解锁密钥依赖、权限不足与其他拒绝分别回报，不自动清除密钥或修改策略。
4. 请求接受后由 Windows 在后台解密，后续用 `status` 跟踪进度。
5. **完成判据**：仅 `conversionStatus === 0` 表示完全解密。`encryptionPercentage === 0` 不能单独证明完成。

## 参数

| 参数 | 约束与默认值 |
|---|---|
| `scope` | `status`（默认）、`unlock`、`decrypt` |
| `drive` | 单盘符如 `C` 或 `C:`；`unlock` 必填，`status` 与 `decrypt` 省略时处理全部可枚举卷 |
| `recoveryPassword` | 仅 `unlock` 可选，8 组 6 位数字用 `-` 分隔；其他 `scope` 禁止传入 |

## 返回字段与枚举

返回结构为 `{ "bitlocker": { ... }, "elapsedMs": N }`。

| 字段 | 说明 |
|---|---|
| 查询级字段 | `scope`、`collectedAt`、`admin`、`source`、`volumes[]`、`notice`，可选 `degraded`、`collectionErrors`、`operation`（异常中断重启后的自动复查） |
| `volumes[]` | `volumeId`、`drive`、`volumeType`、`conversionStatus`、`encryptionPercentage`（%）、`encryptionMethod`、`protectionStatus`、`lockStatus`、`autoUnlockEnabled`（仅数据卷）、`autoUnlockKeysStored`（仅当前系统卷）、`keyProtectors[]: { id, type }`，可选 `collectionErrors`、`operation` |
| `conversionStatus` | `0` 完全解密、`1` 完全加密、`2` 加密中、`3` 解密中、`4` 加密暂停、`5` 解密暂停 |
| `lockStatus` | `0` 已解锁、`1` 已锁定 |
| `operation` | `status`（`accepted` / `rejected` / `skipped` / `cancelled` / `unknown`）、`reason`、`returnCode`、可选 `error`、`credentialSource`（`localInput` / `toolParameter`） |

## 异常处理与边界

1. 遇到 `operation.status === "unknown"`（超时、调用异常或进程中断重启后复查）禁止自动重试，须立即用 `status` 复查真实状态。不承诺已提交的 Windows 操作可通过关闭界面或停止对话撤销。
2. 同卷并发操作会被跨进程锁拒绝；批处理按卷独立报告结果。
3. 仅支持 48 位数值恢复密码解锁，不支持数据卷密码、外部密钥、证书或 `repair-bde`；硬件加密盘需结合 `encryptionMethod` 解释状态。
4. 整次失败（如指定盘符未找到唯一卷或查询全败）返回 `isError: true`；部分采集错误置 `degraded: true`，未枚举到卷或字段 `null` 均不能判定为未加密。单次底层查询超时 60 秒，`elapsedMs` 偏高提示加密服务响应慢。

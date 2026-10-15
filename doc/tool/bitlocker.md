# bitlocker：状态、解锁与解密

Agent 工具支持 TUI 和 Web 本地交互；真实加密卷和实际终端验收待补。实现：[bitlocker.ts](../../agent/home/extensions/diagnostics/bitlocker.ts)，使用引导：[SKILL](../../agent/home/skills/bitlocker/SKILL.md)。

`status` 只读；`unlock` 解锁文件访问，保留卷加密；`decrypt` 关闭 BitLocker，发起完整解密。

容量、物理盘和 SMART 使用 [disk](disk.md)。返回包装与错误约定见 [工具文档](README.md#返回结构)。

## 调用

```js
bitlocker({})
bitlocker({ scope: "status", drive: "C:" })

// TUI 显示独立的隐藏恢复密码输入框
bitlocker({ scope: "unlock", drive: "D:" })

// AI 填参路径；占位文本须替换为用户提供的有效恢复密码
bitlocker({ scope: "unlock", drive: "D:", recoveryPassword: "<48位恢复密码>" })

bitlocker({ scope: "decrypt" })
bitlocker({ scope: "decrypt", drive: "D:" })
```

| 参数 | 适用范围 | 说明 |
|---|---|---|
| `scope` | 全部 | `status`（默认）、`unlock`、`decrypt` |
| `drive` | 全部 | 接受 `C` 或 `C:`，不区分大小写；unlock 必填，其他 scope 省略时处理全部可枚举卷 |
| `recoveryPassword` | unlock | 可选的 48 位恢复密码；省略时等待 TUI 本地隐藏输入 |

恢复密码格式为八组六位数字，用连字符分隔。工具校验格式，是否匹配目标卷由 Windows 判断；其他 scope 拒绝携带此参数。恢复密钥 ID、微软账户登录密码和 Windows PIN 均不能代替恢复密码。

## status：查询

查询指定卷或全部可枚举卷，不改变卷状态。每次调用都重新采集，不使用上一次结果代替实时查询。

指定盘符没有查询记录时，明确报告未找到可查询记录。全部枚举仅表示数据源本次返回的范围，不能据此认定未出现的卷没有加密。

## unlock：解锁

初版使用 `UnlockWithNumericalPassword()`，只支持匹配目标卷的 48 位恢复密码。

1. 实时查询目标卷的锁定状态和可读取的保护器信息。
2. 已解锁时直接返回，跳过凭据输入和解锁接口调用；锁定状态查询失败时保留错误，不发起解锁。
3. 已提供 `recoveryPassword` 时使用工具参数；省略时等待 TUI 本地隐藏输入。
4. 提交前重新确认目标卷标识和锁定状态，再调用解锁接口。
5. 通过 `GetLockStatus()` 复查。只有查询成功且状态为已解锁，才确认解锁完成。

解锁不自动发起完整解密。目标卷没有数值恢复密码保护器、密码格式无效或密码不匹配时，保留 Windows 原始返回码和原因。

### TUI 交互

TUI 使用具有本地 UI 的会话；Web 使用会话绑定的独立交互通道。恢复密码默认隐藏，支持粘贴；Enter 提交、Esc 取消、Ctrl+R 显示或隐藏。输入与确认等待最多三分钟；显示的明文可能被终端录制。

AI 填参路径也在 TUI 确认目标与模型传输边界。工具调用与 Markdown 渲染隐藏正常格式的恢复密码；完整结果可在 TUI 查看。其他模式可查询状态，需要执行时明确报告能力缺失。

### 工具卡片

Web 卡片通过独立待处理接口提供输入和确认。请求绑定会话、调用、固定目标卷和三分钟期限；页面刷新重新读取待处理请求，不恢复已处理请求。凭据提交绕过持久化去重记录。

卡片显示目标盘符、卷标识、可读取的恢复保护器 ID，以及以下控件：

| 控件 | 行为 |
|---|---|
| 恢复密码输入框 | 默认隐藏内容，支持粘贴和临时显示 |
| 解锁 | 直接向本地后端提交凭据，显示执行和复查结果 |
| 取消 | 结束尚未提交的凭据等待 |
| 查找恢复密钥 | 打开微软账户恢复密钥页面，由用户自行登录 |

默认等待路径为：检查卷 → 等待输入 → 解锁中 → 复查 → 返回结果。等待期间通过独立接口读取交互状态；提交后原调用继续执行，不需要向聊天发送凭据。

本地提交绑定当前会话、工具调用和目标卷，限制重复提交。目标卷变化、等待过期或用户取消后，原提交入口失效。卡片缺少本地输入通道时明确返回能力缺失，AI 填参路径仍可使用。

## decrypt：完整解密

省略 `drive` 时处理本次枚举到的全部目标卷；指定盘符时只处理该卷。执行前固定目标卷清单，在 TUI 本地确认界面展示盘符、当前状态和关闭加密的影响，由用户确认。

| 当前状态 | 处理 |
|---|---|
| 完全解密 | 跳过 |
| 解密中 | 返回当前进度，不重复发起 |
| 解密暂停 | 调用 `Decrypt()` 恢复 |
| 完全加密 | 调用 `Decrypt()` 发起解密 |
| 加密中或加密暂停 | 确认界面注明将改变转换方向；确认后调用 `Decrypt()` |
| 锁定 | 报告阻塞；不自动索取凭据或调用 unlock |
| 状态查询失败 | 报告缺失，不根据默认值执行 |

批量操作逐卷处理并报告结果，没有跨卷的整体成功或自动回滚保证。执行前重新核对卷标识，确认后新出现的卷不加入目标清单。缺少本地确认通道时不执行，也不由 AI 参数代替本地确认。同一进程内按卷标识互斥，其他操作占用目标卷时拒绝本次提交；独立 Pi 进程之间未实现卷操作互斥。

`Decrypt()` 立即返回，数据转换在 Windows 后台进行。请求接受后返回当前状态，后续通过 status 查询进度；只有 `GetConversionStatus()` 成功且状态为 `FullyDecrypted` 才确认完成，百分比为 0 不能单独证明完成。

## 返回字段

结果位于 `bitlocker` 对象中。状态、算法和保护器类型保留 Windows 原始枚举值，不根据权限、保护器或百分比推断其他字段。

### 查询级字段

| 字段 | 含义 |
|---|---|
| `scope` | 本次操作 |
| `collectedAt` | 本次状态采集时间，ISO 8601 格式 |
| `admin` | 当前进程是否具有管理员权限 |
| `source` | `Win32_EncryptableVolume` |
| `volumes[]` | 本次目标卷及其结果 |
| `notice` | 数据源、枚举范围、权限与采集限制；不提供故障判断或排查建议 |
| `degraded` / `collectionErrors[]` | 部分采集失败及具体原因 |

### 每个卷的状态字段

三个 scope 都返回可获取的状态。执行后重新查询；复查失败时保留查询错误，不把执行前快照作为执行后状态。

| 字段 | 含义 |
|---|---|
| `volumeId` | Windows 卷标识；执行时用于核对目标 |
| `drive` | 盘符；无盘符时为 null |
| `volumeType` | 原始值：0 操作系统卷、1 固定数据卷、2 可移动卷 |
| `conversionStatus` | 原始值：0 完全解密、1 完全加密、2 加密中、3 解密中、4 加密暂停、5 解密暂停 |
| `encryptionPercentage` | Windows 报告的加密百分比，单位 % |
| `encryptionMethod` | 加密算法的原始枚举值 |
| `protectionStatus` | 原始值：0 保护关闭、1 保护启用、2 状态未知 |
| `lockStatus` | 原始值：0 已解锁、1 已锁定 |
| `autoUnlockEnabled` | 当前环境是否启用该数据卷的自动解锁；不适用时为 null |
| `autoUnlockKeysStored` | 当前运行的系统卷是否存有数据卷自动解锁信息；不适用时为 null |
| `keyProtectors[]` | 每项含 id、type；不包含恢复密码、外部密钥或密钥包 |
| `collectionErrors[]` | 含 source、code、message，定位失败字段或接口 |

查询失败的字段返回 null 并保留错误；接口明确返回的未知枚举值原样保留。不适用的字段在 notice 中说明。实时状态使用 `GetConversionStatus()`、`GetProtectionStatus()` 等方法，避免复用 WMI 对象创建时的属性快照。

### 操作结果字段

unlock、decrypt 在每个目标卷上增加 `operation`，与实际卷状态分开记录。

| 字段 | 含义 |
|---|---|
| `operation.status` | accepted 请求接受、rejected 明确拒绝、skipped 无需操作、cancelled 提交前取消、unknown 无法确认请求结果 |
| `operation.reason` | 已解锁、已解密、解密中、卷锁定、用户取消等原因 |
| `operation.returnCode` | Windows 原始返回码；未调用或未收到返回时为 null |
| `operation.error` | 具体操作错误，含 source、code、message |
| `operation.credentialSource` | unlock 使用 localInput 或 toolParameter；未使用凭据时为 null，decrypt 不返回此字段 |

请求成功但复查失败时保留 accepted，并注明状态缺失。超时或连接中断导致未收到操作返回时标记 unknown，先查询实际状态再决定后续操作，避免盲目重试。

整次查询无法完成时按通用约定标记 `isError`。状态查询和 Windows 操作调用的子进程超时均为 60 秒。有可用状态或批量操作部分成功时保留逐卷结果；单卷失败不影响其他目标的报告。

## 凭据与 SKILL

默认通过 TUI / Web 独立输入，保留 AI 填参能力。

| 输入路径 | 数据流 | 模型是否接触恢复密码 |
|---|---|---|
| TUI / Web 本地输入 | 隐藏输入框 → 独立交互通道 → Agent Tool → Windows 接口 | 否 |
| AI 填参 | 用户消息或模型上下文 → 工具参数 → Windows 接口 | 是 |

本地输入不转换成聊天消息、模型工具参数或工具更新。凭据只在执行所需时间内保留，不写工具自身的会话、日志或遥测，不进入子进程命令行或临时脚本文件；通过标准输入交给 PowerShell。返回值和错误信息均不回显凭据。

Windows 审计策略、终端录制、模型服务商与第三方扩展的记录需单独核查，工具无法保证这些记录不含凭据。

AI 填参通过 message_end 在会话落盘前隐藏恢复密码，按调用 ID 和盘符保存一次性内存凭据。正常格式的聊天文本在落盘前脱敏，仅在本次 Agent 运行的模型请求临时副本中恢复；Agent 运行结束、会话切换或退出后清除内存引用。图像、附件和其他密码格式不在文本脱敏范围内。

脱敏无法消除凭据进入模型上下文并可能发送给模型服务商的事实，使用前须告知用户。Web 的原用户消息还可能留在聊天草稿与输入收件箱，选择 AI 填参时须说明这一边界。需要避免这些记录时使用独立本地输入。

HTTP 会话快照与 SSE 使用脱敏副本，恢复密码参数始终替换为隐藏标记；不向浏览器转发包含原始参数片段的 SDK 增量副本。该转换不改变执行参数和模型上下文。

SKILL 负责以下引导，工具固定说明只承担数据口径与限制：

1. 确认目标盘并查询状态。已解锁时不索取密钥。
2. 锁定时说明需要目标卷对应的 48 位恢复密码，默认引导 TUI 本地输入。
3. 缺少密钥时引导打开 <https://aka.ms/myrecoverykey>，由用户自行登录并按恢复密钥 ID 匹配记录。
4. 微软账户中必须已备份对应密钥。没有记录时检查其他相关账户、单位 IT、保存的文件或打印件，不承诺微软能够重建丢失密钥。
5. 区分解锁与关闭加密；decrypt 只在用户明确要求解除保护并完成本地确认后执行。

## 限制

- 支持范围为 Windows 10/11 x64；WMI 接口要求管理员权限及符合要求的连接。裁剪系统、PE、组件和策略限制需单独验证，提权不保证调用成功。
- 系统卷存有数据卷自动解锁信息时，Decrypt 可能返回 `0x80310029`。即使当前数据卷全部关闭自动解锁，也可能残留已断开的卷的信息；工具不自动清除这些密钥。
- 锁定卷的 Decrypt 返回 `0x80310000`。接口还可能返回其他错误，必须保留原始值，不将错误码列表视为完整清单。
- 关闭卡片或取消等待只能结束尚未提交的请求。请求已交给 Windows 后不能宣称已撤销；解密在后台继续，关机或休眠后通常在下次 Windows 启动时恢复。
- 解密会关闭数据保护，完成后移除密钥保护器。硬件加密卷的 FullyDecrypted 表示永久解锁，需结合 encryptionMethod 说明口径。
- 不支持启动 PIN、数据卷密码、外部密钥文件或证书解锁；初版仅接入数值恢复密码。
- 不自动修改策略、额外删除保护器、调用 repair-bde 或修复损坏磁盘。关闭加密不作为常规故障排查步骤，也不提供数据恢复保证。

## 微软文档

- [Win32_EncryptableVolume：接口与权限](https://learn.microsoft.com/en-us/windows/win32/secprov/win32-encryptablevolume)
- [New-CimSessionOption：DCOM 默认启用 PacketPrivacy](https://learn.microsoft.com/en-us/powershell/module/cimcmdlets/new-cimsessionoption)
- [UnlockWithNumericalPassword：恢复密码解锁](https://learn.microsoft.com/en-us/windows/win32/secprov/unlockwithnumericalpassword-win32-encryptablevolume)
- [Decrypt：后台解密与错误码](https://learn.microsoft.com/en-us/windows/win32/secprov/decrypt-win32-encryptablevolume)
- [GetConversionStatus：完成判据与百分比](https://learn.microsoft.com/en-us/windows/win32/secprov/getconversionstatus-win32-encryptablevolume)
- [IsAutoUnlockKeyStored：系统卷保存的自动解锁信息](https://learn.microsoft.com/en-us/windows/win32/secprov/isautounlockkeystored-win32-encryptablevolume)
- [BitLocker 操作指南](https://learn.microsoft.com/en-us/windows/security/operating-system-security/data-protection/bitlocker/operations-guide)
- [查找恢复密钥](https://support.microsoft.com/en-us/windows/security/encryption/find-your-bitlocker-recovery-key)

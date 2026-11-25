---
name: bitlocker
description: 查询 BitLocker 状态，使用恢复密码解锁卷，或经本地确认关闭加密。说明 status、unlock、decrypt 的参数、TUI / Web 凭据输入、恢复密钥查找、异步进度与失败处理。
---

# BitLocker 工具说明

status 只读；unlock 解锁文件访问并保留加密；decrypt 关闭 BitLocker，发起完整解密。执行操作支持 TUI 和 Web，用户确认或输入之前不会提交 Windows 操作。

## 参数与调用

| 参数 | 说明 |
|---|---|
| scope | status（默认）、unlock、decrypt |
| drive | C 或 C: 等盘符；unlock 必填，其他 scope 省略时处理全部可枚举卷 |
| recoveryPassword | unlock 可选，八组六位数字用连字符分隔；默认省略，交给 TUI / Web 隐藏输入 |

```js
bitlocker({})
bitlocker({ scope: "unlock", drive: "D:" })
bitlocker({ scope: "decrypt", drive: "D:" })
```

## 解锁与恢复密钥

1. 确认目标盘，先查询是否锁定。卷已解锁时无需索取凭据。
2. 用户明确要求解锁后调用 unlock，默认省略 recoveryPassword，让用户在 TUI / Web 独立输入。Enter 提交，Esc 取消，等待最多三分钟。
3. 用户缺少密钥时，引导打开 <https://aka.ms/myrecoverykey>，自行登录相关微软账户，并按工具返回的恢复保护器 ID 匹配记录。初版不自动打开浏览器、不登录账户、不读取网页中的密钥。
4. 微软账户必须已备份对应的 48 位恢复密码。有账户不保证有记录；没有记录时检查其他相关账户、单位 IT、保存的文件或打印件。
5. 恢复密钥 ID 只能用于匹配记录。账户登录密码、Windows PIN 和启动 PIN 均不能充当 recoveryPassword；管理员权限不能替代有效凭据。
6. 用户明确选择 AI 填参时，先说明恢复密码会进入模型上下文并可能发送给模型服务商，再使用 recoveryPassword。不要主动要求用户把密钥发进聊天；工具会再进行本地确认，返回值不回显密码。

解锁结果同时检查 operation 与 lockStatus。只有实时复查成功且 lockStatus=0，才能确认已解锁；解锁成功不表示关闭加密。

## 完整解密

1. 仅在用户明确要求解除加密保护时调用 decrypt。指定盘符仅操作该卷；省略 drive 将处理全部可枚举卷。
2. 工具在本地界面展示固定目标清单、当前转换状态及影响，并要求确认。缺少本地交互通道或用户取消时不执行，不使用其他工具绕过确认。
3. 锁定卷报告阻塞，先安排独立的 unlock 调用。系统卷自动解锁密钥依赖、权限不足和其他失败分别报告，不自动清除密钥或改策略。
4. 请求接受后，Windows 在后台执行。后续用 status 查询进度；仅 conversionStatus=0 表示完全解密，百分比为 0 不能单独证明完成。
5. 批量操作逐卷报告。accepted 表示请求接受，rejected 表示拒绝，skipped 表示无需操作，cancelled 表示提交前取消，unknown 表示请求结果未确认。
6. unknown 时先查状态，不盲目重试。不承诺已提交的操作可以通过关闭输入框或停止 Agent 撤销。

## 返回与限制

- 结果位于 bitlocker 对象中，包含采集时间、管理员权限、volumes、notice 和 collectionErrors。
- 每卷含 volumeId、drive、转换状态与百分比、算法、保护状态、锁定状态、自动解锁信息，以及保护器 ID 和类型。状态值保留 Windows 原始枚举；null 须结合错误判断。
- 转换状态：0 完全解密、1 完全加密、2 加密中、3 解密中、4 加密暂停、5 解密暂停。锁定状态：0 已解锁、1 已锁定。
- Windows 方法返回码和采集错误保留在工具结果中，用于核查。答复只补充影响当前判断或操作的失败；无法查询不表示未加密，接口调用成功不保证已经完成数据转换。
- 初版仅支持数值恢复密码解锁，不使用数据卷密码、外部密钥或证书，不执行 repair-bde。
- 不把关闭加密作为常规故障排查步骤。解密会解除保护，完成后移除保护器；硬件加密盘需结合算法解释状态。

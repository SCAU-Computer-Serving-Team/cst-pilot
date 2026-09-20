# startup：开机自启盘点

列出注册表自启项、启动文件夹和自动启动服务，回答“开机会拉起什么”。当前资源占用使用 [sys](sys.md)。

实现：[startup.ts](../../agent/home/extensions/diagnostics/startup.ts)；独立工具的设计理由见 [sys 设计](../design/tool/sys-design.md)。

## 调用

无参数，一次获取三类配置：

```js
startup({})
```

## 返回

结果位于 `startup` 对象中。

| 字段 | 每项内容 |
|---|---|
| `regItems[]` | `source` 注册表路径、`name`、`command`、`disabled` |
| `startupFolders[]` | `scope`（user/allUsers）、`path`、`items[]`（name/disabled） |
| `services[]` | `name`、`display`、`state`、`path`；Running 排前 |
| `notice` | 范围和字段说明 |
| `collectionErrors` / `degraded` | 部分采集失败时返回 |

```json
{
  "source": "HKCU\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run",
  "name": "Example",
  "command": "\"C:\\Apps\\Example.exe\" -startup",
  "disabled": false
}
```

上例是一条 `regItems` 记录。禁用状态按三态判读：

| disabled | 含义 |
|---|---|
| `true` | 匹配的 StartupApproved 记录表示已禁用 |
| `false` | 匹配记录表示启用 |
| `null` | 无可用的匹配状态，不能据此确认实际启动情况 |

注册表存在某项，不等于它当前正在运行；服务配置为 Auto，也不保证当前为 Running。

## 数据来源

| 来源 | 范围与处理 |
|---|---|
| Run / RunOnce | HKLM/HKCU 的 Run、RunOnce，以及 HKLM Wow6432Node 的 Run |
| StartupApproved | 按注册表根、Run/Run32 类别和名称匹配；不将 Run 状态套到 RunOnce |
| 用户与公共启动文件夹 | 枚举文件，按对应注册表根和完整文件名匹配禁用状态 |
| `Win32_Service` | `StartMode='Auto'`，含延迟自启；服务命令路径超过 140 字符时截断 |

StartupApproved 的禁用信息与 Run 项分开存放，因此不能只列 Run 键。实现按记录首字节的奇偶解释状态，并保留无法匹配的情况。

## 限制

- 不覆盖计划任务、WMI 事件订阅等其他启动或持久化机制。
- 保留系统服务和第三方服务，不以路径或厂商硬过滤；System32 路径本身也不能证明可信。
- 在 PE 中查询的是当前 PE 的注册表、用户和服务，不是离线 Windows 的配置。
- 采集失败会保留错误；空清单须结合错误字段判断。本工具不启用、禁用或删除任何项目。

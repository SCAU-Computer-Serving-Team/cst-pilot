# driver：设备与驱动状态

查看设备是否被识别、驱动状态是否异常，以及蓝牙、音频等服务是否运行。负载和温度使用 [sys](sys.md)，容量和 SMART 使用 [disk](disk.md)。

实现：[driver.ts](../../agent/home/extensions/diagnostics/driver.ts) 与 [driver-core.ts](../../agent/home/extensions/diagnostics/driver-core.ts)；设计见 [driver 设计](../design/tool/driver-design.md)。

## 调用

```js
driver({})
driver({ scope: "core" })
driver({ scope: "external" })
driver({ scope: "find", class: "Net", name: "Realtek" })
driver({ scope: "find", id: "VID_045E" })
```

| 参数 | 适用范围 | 说明 |
|---|---|---|
| `scope` | 全部 | `problem`（默认）、`core`、`external`、`find` |
| `name` | find | 名称的字面子串，不区分大小写 |
| `class` | find | 固定设备类精确匹配，如 Net、MEDIA、Bluetooth |
| `id` | find | DeviceID 或任一 HardwareID 的字面子串，不区分大小写 |

find 至少传一个条件，多个条件按 AND 组合。名称和 ID 不支持正则或通配符。

## 返回

结果按 scope 包装，例如 `{ "problem": { "devices": [], "count": 0, "notice": "..." } }`。部分采集失败时保留 `collectionErrors` 和 `degraded`，不能将此时的空数组解释为没有异常。

### problem 与 find

`devices[]` 每项包含：

| 字段 | 含义 |
|---|---|
| `name` / `class` | 设备名与 PNPClass；可能缺失 |
| `status` | PnP 原始状态 |
| `errorCode` | ConfigManagerErrorCode 原始值 |
| `deviceId` | 设备实例 ID |
| `hardwareIds` | 硬件 ID 数组；用于匹配设备型号 |

problem 查询 `Status=Error/Unknown`，不是所有非零错误码的全集。find 则返回符合条件的设备，不限异常状态。错误码保留原值，工具不内置驱动版本或错误处置对照表。

### core

| 字段 | 主要内容 |
|---|---|
| `net[]` | name、connId、physical、connStatus；保留虚拟网卡 |
| `bluetooth[]` / `audio[]` | name、status、errorCode |
| `display[]` | name、vendor、driver、status、bus |
| `services[]` | bthserv、Audiosrv 的状态 |
| `drivers[]` | class、device、version、date、provider |

网卡只取有 `NetConnectionID` 的条目，避免旧式伪适配器干扰。`physical` 可能由驱动误报，不能单独用来区分实体和虚拟网卡。驱动日期格式为 `yyyy-MM-dd`，仅按 Net、Bluetooth、MEDIA、Display 四类查询签名驱动。

服务缺失只有在采集成功时才可解释为无该服务。驱动版本是否过旧须另行核对，工具只报告版本和日期。

### external

- `devices[]`：DeviceID 以 USB、BTHENUM、DISPLAY 开头的设备；USB 筛选也覆盖 USBSTOR。
- `removable[]`：model、interface、mediaType、sizeGB，来自 USB 或可移动类型的 `Win32_DiskDrive` 记录。

内置摄像头和内置屏也可能在列表中。SD 卡是否出现取决于读卡器的枚举方式，不能仅凭列表名称认定它是外接设备。

## 排查顺序

| 症状 | 建议入口 | 继续核查 |
|---|---|---|
| 蓝牙消失、无声音 | problem → core | 对应设备状态与 bthserv/Audiosrv |
| 无法联网 | core | 网卡连接状态、虚拟网卡；IP 和连通性另查 |
| 外设未识别 | external | 用 find 按名称、VID/PID 或硬件 ID 复查 |
| 触控板、指纹等特定设备异常 | find | 设备 ID、错误码与驱动版本 |

## 限制

数据来自当前系统 WMI/CIM，通常无需管理员。裁剪系统、WMI 故障和缺失字段应通过错误或空值如实呈现；不能保证每种 Windows 10/11 镜像均可采集。

工具不覆盖飞行模式、射频开关、网络打印机、IP 配置、信号强度或网络连通性，也不枚举完整驱动仓库。它不会安装、回滚驱动，启用设备或修改服务。

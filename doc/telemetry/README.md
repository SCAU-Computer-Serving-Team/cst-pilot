# 使用情况遥测

状态：设计中，契约版本 0.1。更新：2026-09-20。

遥测系统是工具包使用数据的收集管线：发送端在队员机器上采集并上报，接收端在队伍服务器上存储，供查询与报表。它回答一个问题：队员的工具包实际用得怎么样。

采什么、不采什么由数据契约决定。本文是入口：系统是什么、有哪些文档、需求与未决项落在哪，不重抄各文档的正文。

| 文件 | 内容 |
|---|---|
| [architecture.md](architecture.md) | 系统架构：组成、会话生命周期、关键决策、技术栈 |
| [../contract.md](../contract.md) | 信息收集契约：共用字段、身份来源、工具字段、计费币种、请求记录、不采集清单 |
| [schema.md](schema.md) | 遥测数据契约：会话记录的字段与口径 |
| [sender/SPEC.md](sender/SPEC.md) | 发送端：采集、上报、身份与凭据、降级、配置 |
| [receiver/SPEC.md](receiver/SPEC.md) | 接收端：接口、存储、报表与运维 |

## 需求

L1–L8 与各文档的议题编号（S / R）不通用。八条都已落成结论，「落点」列指向写结论的地方。

| # | 需求 | 落点 |
|---|---|---|
| L1 | 采集并上报自身使用情况，失败不影响诊断 | architecture「关键决策」容错假设；sender「结论」全程不抛错、不向队员输出 |
| L2 | 只采运行信息，不采机主数据与对话内容 | [../contract.md](../contract.md)「不采集」，turn 级报错原文是唯一例外 |
| L3 | 身份由队员的 OAuth 登录决定，服务端从上传凭据解析 `mid` | [../contract.md](../contract.md)「身份来源」；sender「身份与凭据」 |
| L4 | 可关闭；关闭后不采集、不落盘、不联网 | sender「配置」的 `enabled`；「降级」功能关闭一行 |
| L5 | 本地缓存有上限，超限丢最旧记录 | sender「队列」，`OUTBOX_MAX_RECORDS = 200` |
| L6 | 上报异步，不阻塞会话 | architecture「关键决策」通信通道；时序图不等待结果 |
| L7 | 时间以服务端接收为准，客户端时间仅参考 | [../contract.md](../contract.md) 的 `receivedAt` |
| L8 | 端点随发行流程写入，不写死在源码 | sender「配置」的 `telemetry.json`；architecture「技术栈」 |

## 议题

| 组 | 主题 | 位置 |
|---|---|---|
| S1–S4 | 数据字段与口径 | [../contract.md](../contract.md) |
| R1、R13 | 接收端的部署机器与运维归属 | [receiver/SPEC.md](receiver/SPEC.md) |

发送端已没有未决项，故不列表。已定结论写进正文，未决项留在各文档议题表。

## 等 OAuth 落地的部分

OAuth 尚未实现。下面这些内容等它落地后再讨论定稿，现在按各自文中的暂定规则执行，不阻塞其余开发。

| 位置 | 等什么 |
|---|---|
| [../auth/README.md](../auth/README.md)、[pi-extension.md](../auth/pi-extension.md) | OAuth 登录与令牌方案本身 |
| [../contract.md](../contract.md)「身份来源」 | 令牌怎么换出队员身份 |
| [sender/SPEC.md](sender/SPEC.md)「身份与凭据」 | 凭据的存放位置与字段 |
| [receiver/SPEC.md](receiver/SPEC.md)「身份解析」 | 内省接口，见该文「依赖 OAuth 实现」D1–D5 |

接收端的设计已写完，只剩两类未定项：用什么机器、谁运维，以及上表的部分。后者不阻塞开发，用桩顶替。

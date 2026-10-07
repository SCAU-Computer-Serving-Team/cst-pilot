# fetch_content 工具卡片

项目安装的 `pi-web-access@0.18.0` 支持单 URL、多 URL、`readable/raw/answer` 模式。发行包在 `web-search.json` 中仅放行TUN假IP段 `198.18.0.0/15`，其他私网与重定向检查保留，见[运行注意事项](../../../Notice.md#网页抓取与tun代理)。

## 主模板与字段组

主模板为**原文块**。正文和回答从 `content` 取；多 URL 的 `content` 是摘要，完整正文靠 `responseId` 续读。

| 内容组 | 数据路径 | 展示 |
|---|---|---|
| 抓取概况 · 字段组 | `details.urls/urlCount/successful/responseId/mode/title/totalChars` | 来源、成功数、模式和续读标识 |
| 页面内容 · 原文块 | `content[]` 的文本块 | 保留可读文本、HTML 原文或回答 |
| 图片 · 图片组 | `content[]` 的图片块、`mimeType` | 直接显示图片或视频帧，不根据 `hasImage` 伪造附件 |
| 续读提示 | `details.truncated` 与正文尾部 `offset` | 首屏被截断时保留续读方法 |

单 URL `https://www.iana.org/domains/reserved` 的实测返回字段节选：

| mode | `details` 实测节选 | 正文形式 |
|---|---|---|
| `readable` | `{"urlCount":1,"successful":1,"totalChars":3022,"truncated":false,"hasImage":false}` | 3022 字符可读文本 |
| `raw` | `{"urlCount":1,"successful":1,"totalChars":10497,"truncated":false}` | HTML 原文，以 `<!doctype html>` 开始 |
| `answer` | `{"urlCount":1,"successful":0,"error":"Page answer failed: No current model available for page answering"}` | 该独立测试未提供可用模型 |

多 URL 输入该页与 `https://example.com`，实测 `{"urlCount":2,"successful":2,"totalChars":3171}`；正文仅给两条 URL 的摘要。

## 特殊情况

独立脚本没有当前模型，`answer` 模式实测返回 `content:"Error: Page answer failed: No current model available for page answering"` 与上述 `details.error`，主模板改为错误原文块；这不代表正常 Pi 会话的问答必然失败。

TUN代理可将公开域名解析到 `198.18.0.0/15`；发行配置允许该段，未配置放行策略时工具会报告内网地址检查失败。单 URL 的 `details.error`、多 URL 的 `successful < urlCount` 都须保留失败原因。图片块是代码支持的形态，此次未实测图片 URL。

# 字体分发

Web 分发三档 WOFF2 子集，画布继续使用原 OTF。字体生成仅在开发和 CI 执行，现场不需要 Python 或生成工具。

## 来源与许可

| 项 | 值 |
|---|---|
| 上游 | Adobe Source Han Sans，提交 `a4f7cf94edfb9d7ffbdfc4841de276358bd7e0f2` |
| 原字重 | CN Regular、Medium、Bold |
| 来源校验 | `scripts/prepare-fonts.mjs` 固定每个 OTF 与许可的 SHA-256 |
| 原字体位置 | `src/web/design/fonts/`，缺失时下载固定提交；不入开发仓库 |
| 派生名称 | `CST UI Sans`，内部 name 与 CFF 名称同步修改，避免使用 OFL 保留名 |
| 许可 | SIL OFL 1.1；保留原版权和 LICENSE，附 NOTICE |

字形轮廓与三档字重保持不变。Web CSS 使用派生名称，画布使用原字体名称。

## 字符集与回退

子集包含 GB2312 的 6763 个常用汉字、前端和诊断源码用字，以及 Latin、常用标点、中文标点与全角字符中原字体支持的字形。源码变化自动更新字符集，不读取用户会话。

三档使用同一字符集。未收录汉字、特殊符号与 Emoji 由 `Microsoft YaHei`、系统 sans-serif 补字；不随包提供完整 CJK 字体。罕见字和非简体语言可能出现字体差异。

## 生成

```powershell
python -m pip install -r src/web/frontend/scripts/font-requirements.txt
npm run build --prefix src/web/frontend
npm run test:fonts --prefix src/web/frontend
```

FontTools 4.62.1、Brotli 1.0.9 为固定构建依赖。可通过 `CST_WEB_FONT_PYTHON` 指定 Python；离线来源由 `CST_WEB_FONTS_OFFLINE=1` 控制。

1. 校验或下载原 OTF 与许可。
2. 按稳定文件顺序收集源码字符，合并常用字符集。
3. 子集化、保留排版与许可记录、重命名内部字体，再编码 WOFF2。
4. 验证三档覆盖一致，生成 hash 与字符范围清单；校验成功后发布，移除 public 中的完整 OTF。
5. 配方、来源、字符集和产物 hash 一致时复用缓存，避免重复压缩。同内容文件不重写；Windows 短暂占用时有限重试，失败保留原文件。

## 产物

`public/fonts/` 只包含三档 `CSTUISans-*.woff2`、`LICENSE.txt`、`NOTICE.txt`、`subset.json`。清单记录生成版本、来源提交、配方、字符范围、大小与 hash；不包含用户数据。

发布脚本检查字体清单，发行脚本检查资源和许可。CI 在构建后运行字体测试，确认 WOFF2 头、hash、体积、常用字覆盖与系统回退。

## 体积记录

2026-10-05，字符集 7282 个码位：

| 字重 | 原 OTF 字节 | WOFF2 字节 |
|---|---:|---:|
| Regular | 8,429,224 | 1,424,400 |
| Medium | 8,406,556 | 1,440,272 |
| Bold | 8,569,308 | 1,468,776 |
| 合计 | 25,405,088 | 4,333,448 |

字体减少约 82.9%，约 24.23 MiB 降到 4.13 MiB。后续源码字符改变时以生成清单为准。

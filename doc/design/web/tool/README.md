# 工具卡片设计

[通用骨架与内容组](cards.md)负责组件规则；下表每个工具独立记录返回形态、主模板和实测边界。项目配置 `agent/home/settings.json` 的 `defaultTools` 为 `read`、`ls`，其中 `ls` 由诊断扩展覆盖。扩展包版本以项目配置为准。示例保留普通本机数据；设备序列号、唯一设备标识和账户名、主机名局部用 `x` 遮盖，凭据不入文档。行为规则见[诊断卡片](../../../web/SPEC/diagnostic-cards.md)，画布在 `src/web/design/cst-pilot-tools.pen`。

| 来源 | 工具文档 |
|---|---|
| 诊断扩展 | [disk](disk.md)、[driver](driver.md)、[eventlog](eventlog.md)、[ls](ls.md)、[runbook](runbook.md)、[startup](startup.md)、[sys](sys.md) |
| pi-web-access | [web_search](web_search.md)、[source_check](source_check.md)、[fetch_content](fetch_content.md)、[get_search_content](get_search_content.md) |
| pi-fff | [ffgrep](ffgrep.md)、[fffind](fffind.md) |
| Pi 内置 | [read](read.md) |

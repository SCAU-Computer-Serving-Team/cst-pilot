# runbook：命令清单交付

把需要队员手动执行的命令写成 txt 清单，落在工具包 `outbox\<日期>\` 下。工具只写文件，不执行命令，不修改机主系统。

实现：[runbook.ts](../../agent/home/extensions/diagnostics/runbook.ts)。命令内容的写作规范见 [skills/runbook](../../agent/home/skills/runbook/SKILL.md)，需求见 [PRD D1–D12](../PRD.md)。

## 调用

以下路径为示例。实际清理前需确认内容属于可删除缓存；不同风险档分别生成清单。

```js
runbook({
  title: "清理 C 盘临时文件",
  level: "safe",
  items: [
    {
      summary: "删除当前用户临时文件夹内容，释放 C 盘空间；使用中的临时文件可能报一次错，删除后不可恢复。",
      command: 'Remove-Item "$env:TEMP\\*" -Recurse -Force -ErrorAction SilentlyContinue',
      shell: "powershell",
    },
    {
      summary: "清理已确认无个人文件的示例应用缓存，释放空间；应用可能需要重新下载缓存。",
      command: 'Remove-Item "$env:LOCALAPPDATA\\ExampleApp\\Cache\\*" -Recurse -Force -ErrorAction SilentlyContinue',
      shell: "powershell",
    },
  ],
})
```

| 参数 | 必填 | 说明 |
|---|---|---|
| `title` | 是 | 清单标题，进入文件名；Windows 非法字符会被去掉，上限 48 字 |
| `level` | 是 | `safe` / `low` / `confirm`，决定文件名里的风险档名 |
| `sequence` | 否 | 沿用已有序号修正某一份清单；省略则分配当天递增的新序号 |
| `items[]` | 是 | 命令条目，1–20 条 |
| `items[].summary` | 是 | 一句话说明功能与影响，上限 300 字 |
| `items[].command` | 是 | 命令正文，必须单行，上限 2000 字 |
| `items[].shell` | 是 | `cmd` 或 `powershell` |
| `items[].admin` | 否 | 是否需要管理员权限，默认否 |

## 返回

| 字段 | 说明 |
|---|---|
| `file` | 写入的文件绝对路径 |
| `dir` | 当天的日期目录绝对路径 |
| `name` | 文件名 |
| `sequence` | 本次序号，当天唯一递增 |
| `level` / `levelLabel` | 风险档标识与中文档名 |
| `items` | 条目数 |
| `bytes` | 文件字节数 |
| `encoding` | 固定 `utf-8-bom` |
| `notice` | 写入位置与转述要求 |

```json
{
  "runbook": {
    "file": "E:\\cst-pilot\\outbox\\2026-09-13\\01-安全-清理C盘临时文件.txt",
    "dir": "E:\\cst-pilot\\outbox\\2026-09-13",
    "name": "01-安全-清理C盘临时文件.txt",
    "sequence": 1,
    "level": "safe",
    "levelLabel": "安全",
    "items": 2,
    "bytes": 612,
    "encoding": "utf-8-bom",
    "notice": "清单只写工具包 outbox，未改动机主系统。..."
  }
}
```

## 文件内容

文件头一行概括，条目为环境 / 说明 / 命令三段，命令独占一行，条目之间空行。不使用 Markdown 标记。

```
清理 C 盘临时文件（安全） 2026-09-13 15:42

环境：PowerShell（普通权限）
说明：删除当前用户临时文件夹内容，释放 C 盘空间；使用中的临时文件可能报一次错，删除后不可恢复。
命令：
Remove-Item "$env:TEMP\*" -Recurse -Force -ErrorAction SilentlyContinue

环境：PowerShell（普通权限）
说明：清理已确认无个人文件的示例应用缓存，释放空间；应用可能需要重新下载缓存。
命令：
Remove-Item "$env:LOCALAPPDATA\ExampleApp\Cache\*" -Recurse -Force -ErrorAction SilentlyContinue
```

编码 UTF-8 带 BOM，换行 CRLF，中文系统记事本直接可读。

## 命名与序号

1. 目录按天分：`outbox\<YYYY-MM-DD>\`。
2. 文件名 `<序号>-<风险档>-<标题>.txt`，序号当天唯一递增、跨天从 `01` 重新开始。
3. 一次调用一个文件，一个序号对应一份清单；模型按风险档写三份，就依次占用 01、02、03。
4. 沿用 `sequence` 重写时替换同序号的那一份：文件名跟新标题变，序号不变，文件数不增。
5. 序号分配有模块级串行队列，同一模块实例内的并发调用不会撞号。跨进程，或同一进程里出现多份扩展实例时，序号仍可能重复；按「一个工具包一个实例」的场景未做跨进程互斥。

## 限制

1. 命令含换行、说明或标题为空、参数超长都会被拒绝，整次调用失败并说明原因。
2. 只写工具包 `outbox` 目录，不接受自定义输出路径。
3. 只生成 `.txt`，不生成 `.bat` / `.cmd` / `.ps1`，避免双击即执行。
4. 沿用 `sequence` 修正会先删掉同序号的旧文件再写新的；只读介质下这一步会失败，写入动作本身失败但已删的旧文件不会自动恢复，不影响其他序号的清单。
5. 工具包介质只读、空间不足或目录不可写时返回具体原因，不静默失败。
6. 工具不判断命令的真实风险，分档与取舍由模型按 [SKILL](../../agent/home/skills/runbook/SKILL.md) 决定，队员执行前仍需自行确认。

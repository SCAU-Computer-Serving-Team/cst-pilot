# Pen 操作建议

本文档记录MCP和SKILLS之外的操作建议。

## 建议

1. 首选 pen cli 的操作方式。
2. 让 Pen 桌面应用打开某个 .pen 文件，用 `Start-Process "D:\Pen\Pen.exe" "<绝对路径>"`。
   `pen interactive --app desktop --in <file>` 只声明路径，不会真正打开文件。
3. MCP 需要应用里有一个已打开的文件才能工作，否则报 `A file needs to be open in the editor`。
4. 导出画布节点为图片，用 `execute` 里的 `Export(nodeIds, "png", 目录)`。
   图片按节点 id 命名，不能指定文件名。
   单张图最长边超过 8192 像素时会被等比缩小，长色卡要拆成多个画布再导出。
5. `--enable-preview` 只在发生设计变更时才写预览图。
   headless 模式（`pen interactive --out <file>`）只写 .pen，不导出图片。
6. 新建的 .pen 文件画完节点后，导出图片可能全白：带子节点的 frame 不渲染，不带子节点的正常。
   重启 Pen 应用并重新打开同一文件即可恢复。
7. Pen 会留下多个后台进程。关掉窗口再 `Stop-Process -Name Pen` 往往只杀掉带窗口的那个，残留的
   进程会让下次打开文件时读取旧的内存状态，改了 .pen 文件却看不到效果。
   确认干净用 `Get-Process Pen` 看进程数归零。

8. 通过 MCP/CLI 改完画布后，可用 CLI 直接保存：
   `"save()`nexit()" | pen interactive --app desktop`。
   连上正在运行的应用执行 save()，无需在窗口里按 Ctrl+S。
9. `TakeScreenshot`/`Export` 导出的图片里 icon 节点渲染为问号，是导出缺陷，画布实际正常。
    若画布上也显示问号，是运行实例的图标渲染缓存未刷新，重启应用重新打开文件即可。
    MCP 新建/修改 icon 节点后建议重启验证。
10. MCP `Update` 改 icon 节点时必须带上 `icon`、`library`、`width`、`height` 全部字段，
    只传 `fill` 等部分字段会把节点重置成无名空壳（0×0，Material Symbols Rounded），保存后丢失。

## 来源

1. `pen --help`：CLI 顶层命令、参数与示例。
2. `pen interactive --help`：全部 MCP 工具、参数、调用示例。
3. `pen version`、`pen status`：CLI 版本与登录状态。
4. MCP `pencil_read_skill`：应用内置 skill，含 schema、`execute` API、组件与样式。同一份在 Pen 安装目录的 `resources\app.asar.unpacked\out\skills\pen-dev\`。
5. MCP `pencil_execute` 的 schema：`mcp({ describe: "pencil_execute" })`。
6. `~/.pencil/skills/`：应用下载的设计技能包，排版与动效指导。
7. <https://docs.pen.dev>：官方文档首页。
8. <https://docs.pen.dev/for-developers/the-pen-format>：`.pen` 格式与 schema。
9. <https://docs.pen.dev/for-developers/pen-cli>：CLI 命令、交互模式、批量任务。
10. <https://docs.pen.dev/core-concepts/keyboard-shortcuts>：快捷键，含 `Ctrl+S` 保存、`Ctrl+O` 打开。
11. <https://docs.pen.dev/troubleshooting>：已知限制，含「没有自动保存」。
12. <https://docs.pen.dev/core-concepts/design-libraries>：设计库。
13. <https://docs.pen.dev/core-concepts/components>：组件与实例。
14. `%APPDATA%\Pen\logs\main.log`：应用运行日志，排查 MCP 连接与保存行为。

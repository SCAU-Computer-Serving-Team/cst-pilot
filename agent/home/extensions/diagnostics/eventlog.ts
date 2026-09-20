import { StringEnum } from "@earendil-works/pi-ai";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { runScope, SCOPES } from "./eventlog-core.ts";
import { diagnosticResult, OUTPUT_GUIDELINE, throwOnError } from "./result.ts";

/**
 * eventlog - 只读事件日志工具（cst-pilot 定制）
 *
 * 结构只读：Get-WinEvent 纯查询，不清理、不保存、不改配置。
 * 维修场景的故障线索（意外关机 / 蓝屏 / 崩溃 / 服务失败）都沉淀在事件日志里，
 * 本工具让 pi 直接读取并汇总。单工具多 scope（sys 同构）。
 *
 * 设计：doc/design/tool/event-design.md（里程碑 1-8 已实现，9 收尾待做）。
 * 全部逻辑（参数校验 / 命令构建 / 采集收敛 / 白名单 / scope 路由 / 降级）
 * 在 eventlog-core.ts（不依赖 pi 注册层）；本文件只做注册薄壳。
 *
 * scope 分支（每支自含全部参数，logName 非全局覆盖参数）：
 *   recent   近 N 小时错误/警告汇总（System + Application，level 默认 warn）——无 scope 兜底
 *   boot     意外关机 / 蓝屏 / 开关机历史（kind=all/unexpected/bluescreen，ID 白名单 + WHEA-Logger 多组 OR）
 *   crash    应用崩溃 / 启动失败（1000/1001/1002/1026/33/35，可按程序名消息过滤）
 *   service  服务异常（SCM 白名单 18 个 Error 级 ID，可按服务名消息过滤）
 *   disk     磁盘 / 文件系统报错（7/11/51/129/153/55/98/50/140/157）
 *   security 登录审计 4624/4625/4740（需管理员；非管理员显式预检后 notice 降级，不误报 0 条）
 *   query    自定义结构化查询（ids 下推 + level + provider 正则 / msg 子串后置过滤）
 *   detail   单条详情与原文（recordId 直取 EventRecordID，或 id 取最近一条）
 */

export default function registerEventlog(pi: ExtensionAPI) {
	pi.registerTool({
		name: "eventlog",
		label: "Event Log",
		description:
			"只读事件日志工具，读取机器沉淀的历史故障痕迹，按 scope 选择子功能（不传默认 recent）：recent=近 N 小时错误/警告汇总（开场首选）；boot=开关机/意外关机/蓝屏历史（含 WHEA 硬件错误，ID 白名单内置）；crash=应用崩溃与启动失败（按提供程序与事件 ID 配对；WER/无响应保留原始级别，可按程序名过滤）；service=服务启动失败/挂起/崩溃（可按服务名过滤）；disk=磁盘/文件系统报错与掉盘；security=登录审计 4624/4625/4740（需管理员，非管理员自动降级）；query=按事件 ID/级别/提供程序正则/消息子串自定义查询；detail=按 recordId（或 id 取最近一条）读单条完整原文。与 sys 搭配：sys 看实时负载，eventlog 看历史痕迹。" +
			" 输出 JSON 最多 50 KiB，超限标注 outputTruncated；请缩小查询范围获取省略内容。",
		promptSnippet:
			"Read Windows event logs (read-only): recent errors/warnings, boot/unexpected-shutdown/BSOD history, app crashes, service failures, disk/file-system errors, logon audit, custom queries, and single-event full text",
		promptGuidelines: [
			OUTPUT_GUIDELINE,
			"Use eventlog scope=recent when the user reports odd behavior, crashes, or sluggishness and you need recent error/warning context from the event logs (pairs with sys: sys shows current load, eventlog shows history).",
			"Use scope=boot for 'did it power off / blue screen' questions (unexpected shutdown IDs, BugCheck, WHEA hardware errors); scope=crash for app crashes; scope=service for services failing to start or dying; scope=disk for disk/file-system errors.",
			"Use scope=query with ids/provider/msg to dig for specific events beyond the built-in whitelists; use scope=detail with a recordId (or id) to read one event's full message.",
			"scope=security (logon audit) needs administrator; it degrades with an explicit notice when running non-elevated.",
		],
		parameters: Type.Object({
			scope: Type.Optional(StringEnum(SCOPES)),
			hours: Type.Optional(
				Type.Number({ description: "可选，时间窗（小时），默认 24，上限 720。除 detail 外各 scope 通用。" }),
			),
			top: Type.Optional(
				Type.Number({ description: "可选，事件列表条数上限，默认 100（硬上限 100）。除 detail 外各 scope 通用。" }),
			),
			level: Type.Optional(StringEnum(["warn", "error"] as const)),
			kind: Type.Optional(StringEnum(["all", "unexpected", "bluescreen"] as const)),
			type: Type.Optional(StringEnum(["all", "logonFail", "lockout"] as const)),
			app: Type.Optional(
				Type.String({
					description: 'scope=crash 可选，程序名/模块名模糊过滤（对消息做不区分大小写的子串匹配，如 "chrome"）。',
				}),
			),
			name: Type.Optional(Type.String({ description: "scope=service 可选，服务名模糊过滤（消息子串匹配）。" })),
			provider: Type.Optional(
				Type.String({
					description:
						"scope=query 可选，提供程序正则（不区分大小写，1 秒超时保护，作用于 ProviderName 后置过滤）。",
				}),
			),
			msg: Type.Optional(Type.String({ description: "scope=query 可选，消息子串过滤（不区分大小写）。" })),
			ids: Type.Optional(
				Type.Array(Type.Number(), { description: "scope=query 可选，事件 ID 数组（0-65535 整数，下推过滤）。" }),
			),
			logName: Type.Optional(
				Type.String({
					description:
						'scope=query 可选，单通道名（默认 System+Application，如 "System"、"Microsoft-Windows-PowerShell/Operational"）；scope=detail 必填。',
				}),
			),
			recordId: Type.Optional(
				Type.Number({ description: "scope=detail 与 id 二选一必填，按记录号直取单条完整原文。" }),
			),
			id: Type.Optional(
				Type.Number({ description: "scope=detail 与 recordId 二选一必填，取该事件 ID 最近一条的完整原文。" }),
			),
		}),

		async execute(_toolCallId, params, signal, _onUpdate, _ctx) {
			signal?.throwIfAborted();
			const scope = params.scope ?? "recent";
			const payload = await runScope(params, signal);
			throwOnError(payload);
			const result = { [scope]: payload };
			signal?.throwIfAborted();
			return diagnosticResult(result);
		},
	});
}

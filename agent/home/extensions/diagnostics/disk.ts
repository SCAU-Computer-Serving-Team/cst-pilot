import { StringEnum } from "@earendil-works/pi-ai";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { collectDisk } from "./disk-core.ts";
import { diagnosticResult, OUTPUT_GUIDELINE } from "./result.ts";

export default function registerDisk(pi: ExtensionAPI) {
	pi.registerTool({
		name: "disk",
		label: "Disk Info",
		description:
			"只读磁盘信息工具，按 scope 选择子功能：space=各卷空间；info=物理盘型号/类型/健康状态；health=SMART 寿命/温度/通电小时（需管理员，权限不足自动降级）；usage=目录占用排行（逐文件递归统计，达到预算后返回下界）；all=space+info+health 一次取全。详细指南见 skill「disk」。" +
			" 输出 JSON 最多 50 KiB，超限标注 outputTruncated；请缩小查询范围获取省略内容。",
		promptSnippet: "Query disk space, drive info, health, and directory usage ranking (read-only)",
		promptGuidelines: [
			OUTPUT_GUIDELINE,
			"Use disk when the user asks about disk space, capacity, free space, drive models, drive health, or which folders take the most space.",
			"For usage ranking on large paths (whole drives), expect slower response under normal privileges; results may be lower bounds.",
		],
		parameters: Type.Object({
			scope: StringEnum(["space", "info", "health", "usage", "all"] as const),
			drive: Type.Optional(
				Type.String({ description: 'scope=space/info/health 可选，限定盘符，如 "C" 或 "C:"。省略则返回全部卷。' }),
			),
			path: Type.Optional(
				Type.String({
					description: 'scope=usage 必填。要分析的目录或盘符，如 "C:\\"、"C:\\Users"。整个目录树会被统计。',
				}),
			),
			top: Type.Optional(
				Type.Number({ description: "scope=usage 可选，返回占用最大的前 N 个目录，默认 20，上限 100。" }),
			),
		}),

		async execute(_toolCallId, params, signal, _onUpdate, ctx) {
			const result = await collectDisk(params, ctx.cwd, signal);
			return diagnosticResult(result);
		},
	});
}

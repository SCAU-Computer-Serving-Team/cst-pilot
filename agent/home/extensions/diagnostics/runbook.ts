import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import registerCommandPlans from "./command-plan.ts";
import { diagnosticResult, OUTPUT_GUIDELINE } from "./result.ts";
import { deliverRunbook } from "./runbook-files.ts";

export default function registerRunbook(pi: ExtensionAPI) {
	registerCommandPlans(pi);
	pi.registerTool({
		name: "runbook",
		label: "Command Runbook",
		description:
			"把需要队员手动执行的命令写成 txt 清单，落在工具包 outbox\\<日期>\\ 下（只写工具包，不动机主系统，不执行任何命令）。" +
			"一次调用产出一个文件，序号当天唯一递增；命令按风险分档（safe=安全 / low=较安全 / confirm=需与机主确认），同一档合并成一份清单。" +
			"要改已经写好的某一份，用它的 sequence 重写，不会新增文件。" +
			"每条命令必须是一行、可整行复制，并带一句说明功能与影响。" +
			"中等及以上风险、不可逆且无退路的操作不要写入任何清单。" +
			" 输出 JSON 最多 50 KiB，超限标注 outputTruncated。",
		promptSnippet: "Write a copy-paste command runbook (txt) into the toolkit outbox for the member to run manually",
		promptGuidelines: [
			OUTPUT_GUIDELINE,
			"Use command_plan followed by runbook_deliver for new or revised command plans, including follow-up turns. Use this single-step runbook only when the member explicitly requests immediate TXT delivery or revises a sequence-based file.",
			"Call it once per risk level, not once per command: merge similar operations into one command and put every command of the same level in the same file.",
			"Sequences are unique and increasing within a day. To revise a runbook you already wrote, call again with its sequence; omit sequence to get a new one.",
			"Do not put medium-or-higher risk or irreversible operations into a runbook; explain those in chat instead. Read skills/runbook/SKILL.md before the first call.",
			"Always repeat the returned file path verbatim; the member locates the file from the toolkit outbox folder.",
		],
		parameters: Type.Object({
			title: Type.String({ description: "清单标题，进入文件名，如「清理 C 盘临时文件」" }),
			level: Type.Union([Type.Literal("safe"), Type.Literal("low"), Type.Literal("confirm")], {
				description: "风险档：safe=安全，low=较安全，confirm=需与机主确认",
			}),
			sequence: Type.Optional(
				Type.Integer({
					minimum: 1,
					maximum: 999,
					description: "沿用已有序号修正某一份清单时传入返回值里的 sequence（1-999）；省略则分配当天递增的新序号",
				}),
			),
			items: Type.Array(
				Type.Object({
					summary: Type.String({ description: "一句话说明这条命令做什么、有什么影响" }),
					command: Type.String({ description: "命令正文，必须单行、无 Markdown 标记" }),
					shell: Type.Union([Type.Literal("cmd"), Type.Literal("powershell")], {
						description: "命令在哪个解释器里执行",
					}),
					admin: Type.Optional(Type.Boolean({ description: "是否需要管理员权限，默认否" })),
				}),
				{ minItems: 1, maxItems: 20 },
			),
		}),

		async execute(_toolCallId, params, signal, _onUpdate, _ctx) {
			signal?.throwIfAborted();
			const result = await deliverRunbook(params);
			signal?.throwIfAborted();
			return diagnosticResult(result);
		},
	});
}

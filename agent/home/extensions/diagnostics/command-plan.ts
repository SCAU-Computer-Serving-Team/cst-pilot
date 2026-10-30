import { join } from "node:path";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { ARTIFACTS_DIR, PLAN_DIR } from "./artifact-paths.ts";
import { CommandPlanStore } from "./command-plan-store.ts";
import { diagnosticResult, OUTPUT_GUIDELINE } from "./result.ts";
import { ROOT_DIR } from "./runtime.ts";

const id = Type.String({ description: "command_plan 返回的稳定方案 ID" });
const version = Type.Integer({ minimum: 1, description: "方案版本，使用工具返回值" });

export default function registerCommandPlans(pi: ExtensionAPI): void {
	const store = new CommandPlanStore(ARTIFACTS_DIR, PLAN_DIR, join(ROOT_DIR, "outbox", ".plans"));
	pi.registerTool({
		name: "command_plan",
		label: "Command Plan",
		description:
			"保存或修订供队员手动执行的命令方案，返回完整命令、稳定 ID 与版本。只在工具包 agent/home/command-plans 保存方案，不执行命令。每次提出新命令或修改命令时都调用；已有清单只代表对应版本。需要文本文件时再调用 runbook_deliver。",
		promptSnippet: "Prepare or revise a versioned manual repair command plan",
		promptGuidelines: [
			OUTPUT_GUIDELINE,
			"Read skills/runbook/SKILL.md before planning repair commands. Use command_plan for every new or revised batch of manual repair commands, including follow-up turns. Merge commands of the same risk level into one plan.",
			"To revise, supply the returned runbookId and baseVersion with the complete updated items. On version conflict, inspect the latest plan before retrying. Never invent an ID or silently reuse an old plan for new commands.",
			"Diagnostic data collection uses diagnostic tools. Do not include destructive or system-breaking commands. Explain the plan and get the member's agreement before exporting with runbook_deliver; no tool executes the commands.",
		],
		parameters: Type.Object({
			title: Type.String({ maxLength: 48, description: "方案标题" }),
			level: Type.Union([Type.Literal("safe"), Type.Literal("low"), Type.Literal("confirm")]),
			runbookId: Type.Optional(id),
			baseVersion: Type.Optional(version),
			date: Type.Optional(Type.String({ description: "归档日期 YYYY-MM-DD，默认本机当天；修订沿用原日期" })),
			items: Type.Array(
				Type.Object({
					summary: Type.String({ maxLength: 300, description: "说明动作、影响和管理员权限" }),
					command: Type.String({ maxLength: 2000, description: "可整行复制的单行命令，无 Markdown" }),
					shell: Type.Union([Type.Literal("cmd"), Type.Literal("powershell")]),
					admin: Type.Optional(Type.Boolean()),
				}),
				{ minItems: 1, maxItems: 20 },
			),
		}),
		async execute(_toolCallId, params, signal) {
			signal?.throwIfAborted();
			const plan = await store.plan(params);
			return diagnosticResult({
				runbook: {
					...plan,
					action: "planned",
					items: plan.commands.length,
					notice: "命令方案已保存，尚未导出文本。请核对后用 runbook_deliver 交付此 ID 和版本；命令需手动执行。",
				},
			});
		},
	});
	pi.registerTool({
		name: "runbook_deliver",
		label: "Deliver Runbook",
		description:
			"将已核对的命令方案导出为 UTF-8 文本清单，只需 runbookId 和 version，无需重复命令。返回完整命令和文件路径。同版本重试沿用同一文件；旧版本无法覆盖新版本。只写工具包 artifacts/<日期>/runbook，不执行命令。",
		promptSnippet: "Export an agreed command plan version as a copy-paste text file",
		promptGuidelines: [
			OUTPUT_GUIDELINE,
			"Use the exact ID and version returned by command_plan after the member agrees to the plan. Repeat the returned file path and manual execution requirements. Revisions require command_plan first; do not export stale versions.",
		],
		parameters: Type.Object({ runbookId: id, version }),
		async execute(_toolCallId, params, signal) {
			signal?.throwIfAborted();
			return diagnosticResult({ runbook: await store.deliver(params.runbookId, params.version) });
		},
	});
}

import assert from "node:assert/strict";
import { test } from "node:test";
import type { Message } from "../data/web-state.ts";
import { mapSpecialTool, type SpecialToolCall, specialToolKind } from "./special-tool-data.ts";

const call = (name: string, args: Record<string, unknown> = {}): SpecialToolCall => ({
	type: "toolCall",
	id: "fixture",
	name,
	arguments: args,
});
const result = (details: unknown, isError = false): Message => ({
	role: "toolResult",
	content: [{ type: "text", text: JSON.stringify(details) }],
	timestamp: 1,
	details,
	isError,
});
test("特殊工具范围和默认 scope 与画布相符", () => {
	assert.equal(specialToolKind(call("sys")), "sys-overview");
	assert.equal(specialToolKind(call("sys", { scope: "proc" })), undefined);
	assert.equal(specialToolKind(call("bitlocker")), undefined);
	assert.equal(specialToolKind(call("bitlocker", { scope: "status" })), undefined);
	assert.equal(specialToolKind(call("bitlocker", { scope: "decrypt" })), "bitlocker");
});
test("概览保留零值，采集失败不显示已采集", () => {
	const c = call("sys");
	const v = mapSpecialTool(
		c,
		result({ overview: { cpuTotalPct: 0, mem: { usedPct: 0, totalMB: 32768, usedMB: 0 } } }),
	);
	assert.equal(v?.kind, "sys-overview");
	if (v?.kind === "sys-overview") {
		assert.equal(v.cpu, 0);
		assert.equal(v.memory, 0);
		assert.equal(v.capacity, "0 / 32 GB");
	}
	assert.equal(mapSpecialTool(c, result({ error: "失败" }, true))?.summary, "采集失败");
	assert.equal(
		mapSpecialTool(
			c,
			result({ overview: { degraded: true, collectionErrors: [{ source: "mem", code: 5, message: "不可用" }] } }),
		)?.status,
		"degraded",
	);
});
test("检索零结果成功，部分、全部失败和空错误文本分别处理", () => {
	const c = call("web_search", { queries: ["one", "two"] });
	assert.equal(mapSpecialTool(c, result({ successfulQueries: 2, queryCount: 2, totalResults: 0 }))?.status, "success");
	assert.equal(mapSpecialTool(c, result({ successfulQueries: 1, queryCount: 2 }))?.status, "degraded");
	assert.equal(mapSpecialTool(c, result({ successfulQueries: 0, queryCount: 2 }))?.status, "error");
	assert.equal(mapSpecialTool(c, result({ error: "service unavailable" }))?.status, "error");
});
test("三档清单共用组件，失败无文件链接，历史命令来自调用", () => {
	for (const level of ["safe", "low", "confirm"]) {
		const c = call("runbook", {
			title: "测试",
			level,
			items: [{ shell: "cmd", admin: true, summary: "说明", command: "echo historical" }],
		});
		const v = mapSpecialTool(
			c,
			result({ runbook: { name: "a.txt", file: "E:/outbox/a.txt", levelLabel: level, items: 1 } }),
		);
		if (v?.kind === "runbook") {
			assert.equal(v.commands[0]?.command, "echo historical");
			assert.equal(v.delivery?.level, level);
		}
		const failed = mapSpecialTool(c, result({ error: "readonly" }, true));
		assert.equal(failed?.status, "error");
		assert.equal(failed?.kind === "runbook" && failed.delivery, undefined);
	}
});
test("清单以结果中的标题和命令为准，调用缺参也能恢复", () => {
	const commands = [{ shell: "powershell", admin: false, summary: "输出新内容", command: "Write-Output latest" }];
	const v = mapSpecialTool(
		call("runbook", { title: "旧标题", items: [{ command: "stale" }] }),
		result({ runbook: { title: "修订后清单", commands, file: "E:/outbox/a.txt", items: 1 } }),
	);
	assert.equal(v?.title, "修订后清单");
	assert.deepEqual(v?.kind === "runbook" && v.commands, commands);
	const restored = mapSpecialTool(call("runbook"), result({ runbook: { title: "修订后清单", commands } }));
	assert.deepEqual(restored?.kind === "runbook" && restored.commands, commands);
});
test("方案和交付使用相同命令卡，文本结果也能独立恢复", () => {
	const commands = [{ shell: "cmd", admin: false, summary: "说明", command: "echo restored" }];
	for (const name of ["command_plan", "runbook_deliver"]) {
		const details = {
			runbook: {
				title: "新方案",
				version: 2,
				levelLabel: "安全",
				commands,
				items: 1,
				action: name === "command_plan" ? "planned" : "delivered",
				...(name === "runbook_deliver" ? { file: "E:/outbox/plan.txt" } : {}),
			},
		};
		const message = result(details);
		delete message.details;
		const view = mapSpecialTool(call(name), message);
		assert.equal(view?.kind, "runbook");
		if (view?.kind !== "runbook") throw new Error("命令卡缺失");
		assert.deepEqual(view.commands, commands);
		assert.equal(view.summary, name === "command_plan" ? "方案已保存" : "已生成");
	}
});

test("BitLocker 请求、实际状态、取消与复查缺失独立映射", () => {
	const c = call("bitlocker", { scope: "unlock", drive: "D:" });
	const v = (operation: string, lockStatus: number | null = 0, extra = {}) =>
		result({
			bitlocker: {
				volumes: [
					{
						volumeId: "v",
						drive: "D:",
						conversionStatus: 1,
						lockStatus,
						operation: { status: operation, reason: "fixture" },
						...extra,
					},
				],
			},
		});
	assert.equal(mapSpecialTool(c, v("accepted"))?.summary, "已解锁");
	assert.equal(mapSpecialTool(c, v("cancelled", 1))?.status, "cancelled");
	assert.equal(mapSpecialTool(c, v("rejected", 1))?.status, "error");
	assert.equal(mapSpecialTool(c, v("unknown", null))?.status, "unknown");
	const failed = mapSpecialTool(
		c,
		v("accepted", null, { collectionErrors: [{ source: "GetLockStatus", code: 5, message: "不可用" }] }),
	);
	assert.equal(failed?.status, "degraded");
	assert.ok(failed?.errors[0]?.includes("0x00000005"));
});
test("缺失主要读数保持降级，盘符展示规范且无效参数不回显", () => {
	assert.equal(
		mapSpecialTool(call("sys"), result({ overview: { cpuTotalPct: null, mem: null } }))?.status,
		"degraded",
	);
	assert.equal(mapSpecialTool(call("bitlocker", { scope: "unlock", drive: "d" }))?.title, "解锁 D:");
	assert.equal(mapSpecialTool(call("bitlocker", { scope: "unlock", drive: "invalid" }))?.title, "解锁 目标待确定");
});
test("运行文案读取真实工具阶段，不推断卷转换进度", () => {
	const c = call("bitlocker", { scope: "unlock", drive: "D:" });
	c.execution = { startedAt: 1, stage: "正在解锁并复查状态" };
	assert.equal(mapSpecialTool(c, undefined, true)?.summary, "解锁中");
	c.execution.stage = "unknown";
	assert.equal(mapSpecialTool(c, undefined, true)?.summary, "执行中");
});
test("无结构化检索结果保留原文并提示状态未确认", () => {
	const v = mapSpecialTool(call("web_search", { query: "fixture" }), {
		role: "toolResult",
		content: "原始检索返回",
		timestamp: 1,
	});
	assert.equal(v?.status, "degraded");
	assert.equal(v?.raw, "原始检索返回");
	assert.equal(v?.kind === "web-search" && v.unstructured, true);
	const failed = mapSpecialTool(call("web_search"), result({ error: { status: 429, description: "quota" } }));
	assert.ok(failed?.errors[0]?.includes("quota"));
});
test("批量取消不能因已有解密任务或跳过项变成成功", () => {
	const c = call("bitlocker", { scope: "decrypt" });
	const volume = (id: string, operation: string, conversionStatus: number) => ({
		volumeId: id,
		drive: id,
		lockStatus: 0,
		conversionStatus,
		operation: { status: operation },
	});
	const cancelled = mapSpecialTool(
		c,
		result({
			bitlocker: { volumes: [volume("D:", "cancelled", 1), volume("E:", "skipped", 3), volume("F:", "skipped", 0)] },
		}),
	);
	assert.equal(cancelled?.status, "cancelled");
	const partial = mapSpecialTool(
		c,
		result({ bitlocker: { volumes: [volume("D:", "accepted", 3), volume("E:", "cancelled", 1)] } }),
	);
	assert.equal(partial?.status, "degraded");
	assert.match(partial?.summary ?? "", /取消/);
});
test("解密比例为零不能代替完全解密状态", () => {
	const c = call("bitlocker", { scope: "decrypt" });
	const r = (conversionStatus: number) =>
		result({
			bitlocker: {
				volumes: [
					{
						volumeId: "v",
						drive: "D:",
						conversionStatus,
						lockStatus: 0,
						encryptionPercentage: 0,
						operation: { status: "accepted" },
					},
				],
			},
		});
	assert.equal(mapSpecialTool(c, r(3))?.summary, "解密中");
	assert.equal(mapSpecialTool(c, r(0))?.summary, "已完全解密");
});

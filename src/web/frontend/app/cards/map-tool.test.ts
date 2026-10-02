import assert from "node:assert/strict";
import { test } from "node:test";
import type { ContentPart, Message } from "../app/web-state.ts";
import { mapTool } from "./map-tool.ts";

const call = (name: string, scope?: string): Extract<ContentPart, { type: "toolCall" }> => ({
	type: "toolCall",
	id: "tool-1",
	name,
	arguments: scope ? { scope } : {},
});
const result = (details: unknown, text = "原始内容", isError = false): Message =>
	({ role: "toolResult", timestamp: 1, content: [{ type: "text", text }], isError, details }) as Message;

const fixtures: [string, string | undefined, unknown, string][] = [
	["disk", "space", { space: [{ drive: "E:", freeGB: 64.9 }] }, "卷 E"],
	["disk", "usage", { usage: { root: "E:\\logs", topDirs: [{ path: "E:\\logs", sizeGB: 10 }] } }, "扫描根"],
	["sys", "overview", { overview: { cpuTotalPct: 19, mem: { freeMB: 5343, usedPct: 83.6 } } }, "内存占用"],
	["driver", "problem", { problem: { devices: [], count: 0 } }, "本次没有结果"],
	[
		"eventlog",
		"recent",
		{ recent: { truncated: true, total: 12315, events: [{ provider: "WHEA", id: 17 }] } },
		"事件 ID",
	],
	["startup", undefined, { startup: { regItems: [{ name: "SecurityHealth", disabled: null }] } }, "注册表自启"],
	["ls", undefined, { totalChildren: 1, entries: [{ name: "cards.md", size: "11 KB" }] }, "直接子项"],
	[
		"runbook",
		undefined,
		{ runbook: { file: "E:\\outbox\\1.txt", sequence: 1, levelLabel: "安全", items: 1, encoding: "utf-8-bom" } },
		"风险档",
	],
	[
		"source_check",
		undefined,
		{ artifact: { claims: [{ status: "unclear" }], sources: [{ title: "RFC", rank: 1 }] } },
		"主张判断",
	],
	["fetch_content", undefined, { urlCount: 1, successful: 1 }, "原始内容"],
	["get_search_content", undefined, { nextOffset: 180, truncated: true }, "截断"],
	["ffgrep", undefined, { totalMatched: 2 }, "原始内容"],
	["fffind", undefined, { totalMatched: 16, hasMore: true }, "原始内容"],
	["read", undefined, undefined, "原始内容"],
];

for (const [name, scope, details, expected] of fixtures) {
	test(`${name}${scope ? ` ${scope}` : ""} preserves its mapped result`, () => {
		const view = mapTool(call(name, scope), result(details));
		assert.equal(view.status, "success");
		assert.match(JSON.stringify(view.blocks), new RegExp(expected));
	});
}

test("diagnostic object lists use grouped field rows, while rankings stay compact", () => {
	const disk = mapTool(call("disk", "space"), result({ space: [{ drive: "C:", freeGB: 40, fileSystem: "NTFS" }] }));
	assert.deepEqual(
		disk.blocks.filter((block) => block.kind === "fields").map((block) => block.title),
		["卷 C:"],
	);
	assert.match(JSON.stringify(disk.blocks), /剩余/);
	const ranked = mapTool(call("sys", "proc"), result({ proc: { byCpu: [{ name: "node", cpuPct: 20 }] } }));
	assert.equal(
		ranked.blocks.some((block) => block.kind === "listing" && block.items[0]?.name === "node"),
		true,
	);
});

test("read image displays the image ahead of its absolute path", () => {
	const imageResult = result(undefined, "Read image file [image/png]");
	imageResult.content = [
		{ type: "text", text: "Read image file [image/png]" },
		{ type: "image", mimeType: "image/png", data: "AA==" },
	];
	const view = mapTool({ ...call("read"), arguments: { path: "E:\\tmp\\image.png" } }, imageResult);
	assert.equal(view.blocks[0].kind, "image");
	assert.equal(view.blocks[1].kind, "fields");
});

test("tool errors do not display structured success data", () => {
	const view = mapTool(call("disk", "health"), result({ smart: [{ Wear: 10 }] }, "SMART denied", true));
	assert.equal(view.status, "error");
	assert.equal(view.blocks.length, 1);
	assert.match(JSON.stringify(view.blocks), /SMART denied/);
});

test("degraded results retain useful data and warnings", () => {
	const view = mapTool(
		call("disk", "all"),
		result({ space: [{ drive: "C:", freeGB: 40 }], smart: null, smartNotice: "需要管理员权限", degraded: true }),
	);
	assert.equal(view.status, "degraded");
	assert.match(JSON.stringify(view.blocks), /需要管理员权限/);
	assert.match(JSON.stringify(view.blocks), /剩余/);
});

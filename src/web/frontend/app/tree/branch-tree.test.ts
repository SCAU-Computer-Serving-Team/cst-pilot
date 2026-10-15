import assert from "node:assert/strict";
import { test } from "node:test";
import {
	applyFold,
	buildVisibleTree,
	collectToolCalls,
	foldable,
	layoutTree,
	matchesSearch,
	rowText,
	slotMark,
	type TreeEntry,
	type TreeNode,
	visibleByDefault,
	visibleByFilter,
} from "./branch-tree.ts";

const entry = (id: string, overrides: Partial<TreeEntry> = {}): TreeEntry => ({
	id,
	parentId: null,
	type: "message",
	timestamp: `2026-09-30T00:00:${String(id.replace(/\D/g, "") || "0").padStart(2, "0")}Z`,
	...overrides,
});
const user = (id: string, text: string): TreeEntry => entry(id, { message: { role: "user", content: text } });
const assistant = (id: string, text: string, stopReason = "stop"): TreeEntry =>
	entry(id, { message: { role: "assistant", content: text, stopReason } });
const toolResult = (id: string, callId: string, toolName: string): TreeEntry =>
	entry(id, { message: { role: "toolResult", toolCallId: callId, toolName, content: "ok" } });
const node = (value: TreeEntry, children: TreeNode[] = []): TreeNode => ({ entry: value, children });

// r →(a 无正文, 隐藏)→ b → c → d(分支点) → e → f(叶)；d 的另一支 g → h
const tree = (): TreeNode[] => [
	node(user("1", "你好"), [
		node(assistant("2", ""), [
			node(assistant("3", "读一遍结构"), [
				node(user("4", "继续"), [
					node(assistant("5", "两个方向"), [
						node(user("6", "走这边"), [node(assistant("7", "完成"))]),
						node(user("8", "另一边"), [node(assistant("9", "那边的回答"))]),
					]),
				]),
			]),
		]),
	]),
];

test("默认视图隐藏无正文 assistant，其子条目挂到最近可见祖先", () => {
	assert.equal(visibleByDefault(assistant("x", ""), null), false);
	assert.equal(visibleByDefault(assistant("x", ""), "x"), true);
	assert.equal(visibleByDefault(assistant("x", "", "error"), null), true);
	assert.equal(visibleByDefault(entry("y", { type: "label" }), null), true);
	assert.equal(visibleByFilter(node(entry("y", { type: "label" }), []), "default"), false);
	const roots = buildVisibleTree(tree(), "7");
	assert.equal(roots.length, 1);
	assert.equal(roots[0].children[0].entry.id, "3");
});

test("列形态：连接符列是方块，其余是竖线或留空", () => {
	const rows = layoutTree(buildVisibleTree(tree(), "7"), "7");
	assert.deepEqual(slotMark(rows[0], 0), { kind: "empty" });
	assert.deepEqual(slotMark(rows[4], 0), { kind: "square" });
	assert.deepEqual(slotMark(rows[5], 0), { kind: "line" });
	assert.deepEqual(slotMark(rows[5], 1), { kind: "empty" });
	assert.deepEqual(slotMark(rows[6], 0), { kind: "square" });
	assert.deepEqual(slotMark(rows[7], 0), { kind: "line-hidden" });
});

test("可折叠行 = 有可见子条目且是根或段首；折叠隐藏全部可见后代", () => {
	const rows = layoutTree(buildVisibleTree(tree(), "7"), "7");
	// 分支点自己（第 5 行）不在段首位置上，不可折叠；它的两个子条目才是段首。
	assert.deepEqual(
		rows.map((row) => foldable(row)),
		[true, false, false, false, true, false, true, false],
	);
	const folded = applyFold(rows, new Set(["6"]));
	assert.deepEqual(
		folded.visible.map((row) => row.node.entry.id),
		["1", "3", "4", "5", "6", "8", "9"],
	);
	assert.equal(folded.hiddenCounts.get("6"), 1);
	const all = applyFold(rows, new Set(["1"]));
	assert.deepEqual(
		all.visible.map((row) => row.node.entry.id),
		["1"],
	);
	assert.equal(all.hiddenCounts.get("1"), 7);
});

test("行文案与搜索", () => {
	const roots = tree();
	const calls = collectToolCalls(roots);
	const withTool = node(assistant("a1", "", "toolUse"), [node(toolResult("t1", "c1", "ls"))]);
	calls.set("c1", { name: "ls", arguments: { path: ".git" } });
	assert.deepEqual(rowText(node(user("u", "你好\n世界")), calls), { kind: "user", role: "user:", text: "你好 世界" });
	assert.equal(rowText(node(assistant("x", "")), calls).text, "（无文本回复）");
	assert.equal(rowText(withTool.children[0], calls).text, "[ls] ok");
	// 结果为空时回退到参数摘要。
	const emptyResult = node(
		entry("t2", { message: { role: "toolResult", toolCallId: "c1", toolName: "ls", content: [] } }),
	);
	assert.equal(rowText(emptyResult, calls).text, "[ls] .git");
	assert.equal(
		rowText(node(entry("k", { type: "compaction", tokensBefore: 182000 })), calls).text,
		"压缩 · 182k tokens",
	);
	assert.equal(
		rowText(node(entry("s", { type: "branch_summary", summary: "已完成核对" })), calls).text,
		"分支总结 · 已完成核对",
	);
	assert.equal(
		matchesSearch(node(user("u", "整理 W19 的结论")), rowText(node(user("u", "整理 W19 的结论")), calls), "w19 结论"),
		true,
	);
	assert.equal(
		matchesSearch(node(user("u", "整理 W19 的结论")), rowText(node(user("u", "整理 W19 的结论")), calls), "没有的词"),
		false,
	);
});

test("视图过滤四档：无工具、仅用户、仅标记、全部", () => {
	const roots = [
		node(user("u1", "开场"), [
			node(assistant("a1", "收到"), [
				node(toolResult("t1", "c1", "ls")),
				node(entry("m1", { type: "model_change", modelId: "m" })),
				node(user("u2", "继续"), [node(assistant("a2", "收尾"))]),
			]),
		]),
	];
	const labeled = [{ ...roots[0], label: "重点" }];
	// 无工具：藏工具结果与设置类，消息保留。
	let visible = buildVisibleTree(roots, null, "no-tools").flatMap(function flatten(n): TreeEntry[] {
		return [n.entry, ...n.children.flatMap(flatten)];
	});
	assert.deepEqual(
		visible.map((item) => item.id),
		["u1", "a1", "u2", "a2"],
	);
	// 仅用户：只剩用户消息，子条目重挂到最近可见祖先。
	visible = buildVisibleTree(roots, null, "user-only").flatMap(function flatten(n): TreeEntry[] {
		return [n.entry, ...n.children.flatMap(flatten)];
	});
	assert.deepEqual(
		visible.map((item) => item.id),
		["u1", "u2"],
	);
	// 仅标记：无标记条目时整棵树为空。
	assert.equal(buildVisibleTree(roots, null, "labeled-only").length, 0);
	assert.deepEqual(
		buildVisibleTree(labeled, null, "labeled-only").map((item) => item.entry.id),
		["u1"],
	);
	// 全部：设置类与工具结果都可见。
	visible = buildVisibleTree(roots, null, "all").flatMap(function flatten(n): TreeEntry[] {
		return [n.entry, ...n.children.flatMap(flatten)];
	});
	assert.deepEqual(
		visible.map((item) => item.id),
		["u1", "a1", "t1", "m1", "u2", "a2"],
	);
	assert.equal(visibleByFilter(node(user("u", "hi")), "default"), true);
});

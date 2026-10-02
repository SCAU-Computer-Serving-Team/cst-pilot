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
} from "./branch-tree.ts";
import { sampleTree } from "./branch-tree-sample.ts";

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
	assert.equal(visibleByDefault(entry("y", { type: "label" }), null), false);
	const roots = buildVisibleTree(tree(), "7");
	assert.equal(roots.length, 1);
	assert.equal(roots[0].children[0].entry.id, "3");
});

test("缩进、连接符与竖线：对齐 TUI flattenTree", () => {
	const rows = layoutTree(buildVisibleTree(tree(), "7"), "7");
	assert.deepEqual(
		rows.map((row) => row.node.entry.id),
		["1", "3", "4", "5", "6", "7", "8", "9"],
	);
	// 单子链条持平；分支点的子条目 +1；段首的第一代再 +1。
	assert.deepEqual(
		rows.map((row) => row.indent),
		[0, 0, 0, 0, 1, 2, 1, 2],
	);
	// 连接符只画在分支点的子条目上。
	assert.deepEqual(
		rows.map((row) => row.showConnector),
		[false, false, false, false, true, false, true, false],
	);
	assert.deepEqual(
		rows.filter((row) => row.showConnector).map((row) => row.connectorColumn),
		[0, 0],
	);
	assert.deepEqual(rows[4].gutters, []);
	// 非末位兄弟的子树带竖线；末位兄弟的子树留空。
	assert.deepEqual(rows[5].gutters, [{ column: 0, show: true }]);
	assert.deepEqual(rows[6].gutters, []);
	assert.deepEqual(rows[7].gutters, [{ column: 0, show: false }]);
	assert.deepEqual(
		rows.map((row) => row.subtreeEnd),
		[7, 7, 7, 7, 5, 5, 7, 7],
	);
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

test("多根按虚拟根处理：整体左移一级且不画连接符", () => {
	const roots = [node(user("1", "第一条"), [node(assistant("2", "回复一"))]), node(user("3", "第二条"))];
	const rows = layoutTree(roots, "2");
	assert.deepEqual(
		rows.map((row) => row.node.entry.id),
		["1", "2", "3"],
	);
	assert.deepEqual(
		rows.map((row) => row.displayIndent),
		[0, 1, 0],
	);
	assert.deepEqual(
		rows.map((row) => row.showConnector),
		[false, false, false],
	);
	assert.deepEqual(
		rows.map((row) => row.indent),
		[1, 2, 1],
	);
});

test("示例数据：分叉、隐藏条目与当前叶位置", () => {
	const { tree: roots, leafId } = sampleTree();
	const rows = layoutTree(buildVisibleTree(roots, leafId), leafId);
	const ids = rows.map((row) => row.node.entry.id);
	// 隐藏条目不占行：设置类与无正文助手。
	assert.equal(rows.length, 20);
	assert.ok(!ids.includes("h3") && !ids.includes("h4") && !ids.includes("h5"));
	// 无正文助手被隐藏，它调用的工具结果挂到 u5 之下。
	assert.equal(ids[15], "t4");
	assert.equal(rows[15].parentIndex, ids.indexOf("u5"));
	// 根到首个分支点是单子链条，全部无方块无竖线。
	assert.deepEqual(
		rows.slice(0, 12).map((row) => row.indent),
		Array(12).fill(0),
	);
	assert.deepEqual(
		rows.slice(0, 12).map((row) => row.showConnector),
		Array(12).fill(false),
	);
	// 分支点的两个子条目各占同一列的一个方块，中间用竖线连接。
	assert.deepEqual(
		rows.map((row) => row.indent),
		[...Array(12).fill(0), 1, 2, 2, 2, 2, 2, 1, 2],
	);
	assert.deepEqual(
		rows.filter((row) => row.showConnector).map((row) => row.node.entry.id),
		["u4", "u6"],
	);
	assert.deepEqual(
		rows.filter((row) => row.showConnector).map((row) => row.connectorColumn),
		[0, 0],
	);
	assert.deepEqual(
		rows.slice(13, 18).map((row) => slotMark(row, 0)),
		Array(5).fill({ kind: "line" }),
	);
	assert.deepEqual(slotMark(rows[19], 0), { kind: "line-hidden" });
	// 末位兄弟的子树里，这一列不画线。
	assert.deepEqual(rows[18].gutters, []);
	// 可折叠行：根与各段首。
	assert.deepEqual(
		rows.filter((row) => foldable(row)).map((row) => row.node.entry.id),
		["u1", "u4", "u6"],
	);
	// 当前叶在中间，后面还有同级分支。
	const leaf = rows.findIndex((row) => row.node.entry.id === leafId);
	assert.equal(leaf, 16);
	assert.ok(leaf < rows.length - 1);
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

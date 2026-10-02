// 与 pi TUI 的排版对照。同一棵树分别交给 TUI 的 TreeList 与本文件的 layoutTree，比较可见行、
// 缩进、连接符列与竖线列。TUI 是唯一真源，这里失败就说明移植走样。
// 依赖 pi 发行包里未导出的内部组件，路径按 @earendil-works/pi-coding-agent 版本固定。
import assert from "node:assert/strict";
import { test } from "node:test";
import { buildVisibleTree, layoutTree, type TreeEntry, type TreeNode } from "./branch-tree.ts";

const DIST = new URL(
	"../../../../../agent/node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/",
	import.meta.url,
);

type Layout = { id: string; indent: number; square: number | null; lines: number[]; blanks: number[] };

/** 交给 TUI 渲染，读回它算出的排版字段。 */
async function tuiLayout(tree: TreeNode[], leafId: string | null) {
	const { TreeSelectorComponent } = await import(new URL("components/tree-selector.js", DIST).href);
	const { initTheme } = await import(new URL("theme/theme.js", DIST).href);
	initTheme("dark");
	const component = new TreeSelectorComponent(
		tree,
		leafId,
		80,
		() => {},
		() => {},
		async () => {},
	);
	const list = component.treeList as {
		multipleRoots: boolean;
		filteredNodes: {
			node: { entry: { id: string } };
			indent: number;
			showConnector: boolean;
			isVirtualRootChild: boolean;
			gutters: { position: number; show: boolean }[];
		}[];
	};
	const rows: Layout[] = list.filteredNodes.map((flat) => {
		const displayIndent = list.multipleRoots ? Math.max(0, flat.indent - 1) : flat.indent;
		const shown = flat.showConnector && !flat.isVirtualRootChild;
		return {
			id: flat.node.entry.id,
			indent: displayIndent,
			square: shown ? Math.max(0, displayIndent - 1) : null,
			lines: flat.gutters.filter((gutter) => gutter.show).map((gutter) => gutter.position),
			blanks: flat.gutters.filter((gutter) => !gutter.show).map((gutter) => gutter.position),
		};
	});
	const text = component.render(150).map((line: string) => line.replace(/\u001b\[[0-9;]*m/g, ""));
	return { rows, text };
}

/** 本项目前端算出的排版。 */
function webLayout(tree: TreeNode[], leafId: string | null): Layout[] {
	return layoutTree(buildVisibleTree(tree, leafId), leafId).map((row) => ({
		id: row.node.entry.id,
		indent: row.displayIndent,
		square: row.showConnector ? row.connectorColumn : null,
		lines: row.gutters.filter((gutter) => gutter.show).map((gutter) => gutter.column),
		blanks: row.gutters.filter((gutter) => !gutter.show).map((gutter) => gutter.column),
	}));
}

async function assertSameLayout(tree: TreeNode[], leafId: string | null, name: string) {
	const tui = await tuiLayout(tree, leafId);
	const web = webLayout(tree, leafId);
	assert.deepEqual(
		{ rows: web },
		{ rows: tui.rows },
		`${name} 与 TUI 排版不一致。TUI 渲染：\n${tui.text.join("\n")}\n\nTUI 排版：\n${JSON.stringify(tui.rows, null, 1)}\n\n前端排版：\n${JSON.stringify(web, null, 1)}`,
	);
}

let clock = 0;
const at = () => new Date(Date.UTC(2026, 0, 1, 0, 0, ++clock)).toISOString();
const entry = (
	id: string,
	parentId: string | null,
	rest: Omit<TreeEntry, "id" | "parentId" | "timestamp">,
): TreeNode => ({
	entry: { id, parentId, timestamp: at(), ...rest },
	children: [],
});
const user = (id: string, parentId: string | null, text: string) =>
	entry(id, parentId, { type: "message", message: { role: "user", content: [{ type: "text", text }] } });
const assistant = (id: string, parentId: string | null, text: string) =>
	entry(id, parentId, { type: "message", message: { role: "assistant", content: [{ type: "text", text }] } });
const toolResult = (id: string, parentId: string | null, text: string) =>
	entry(id, parentId, {
		type: "message",
		message: { role: "toolResult", toolName: "read", toolCallId: "c1", content: [{ type: "text", text }] },
	});
const modelChange = (id: string, parentId: string | null) =>
	entry(id, parentId, { type: "model_change", modelId: "m" });
const link = (parent: TreeNode, ...children: TreeNode[]) => {
	parent.children = children;
	return parent;
};

test("单子链不缩进、不画标记", async () => {
	const a2 = assistant("a2", "u2", "第二段回答");
	const tree = [
		link(user("u1", null, "开场"), link(assistant("a1", "u1", "收到"), link(user("u2", "a1", "继续"), a2))),
	];
	await assertSameLayout(tree, "a2", "单子链");
	assert.deepEqual(
		webLayout(tree, "a2").map((row) => row.indent),
		[0, 0, 0, 0],
	);
});

test("分支点的子条目进一级，第一代的下一代再进一级", async () => {
	const a1 = assistant("a1", "u1", "收到");
	const u2 = user("u2", "a1", "分支 A");
	const u3 = user("u3", "a1", "分支 B");
	const tree = [
		link(
			user("u1", null, "开场"),
			link(a1, link(u2, assistant("a2", "u2", "A 的回答")), link(u3, assistant("a3", "u3", "B 的回答"))),
		),
	];
	await assertSameLayout(tree, "a2", "一段分支");
	assert.deepEqual(
		webLayout(tree, "a2").map((row) => [row.id, row.indent, row.square, row.lines, row.blanks]),
		[
			["u1", 0, null, [], []],
			["a1", 0, null, [], []],
			["u2", 1, 0, [], []],
			["a2", 2, null, [0], []],
			["u3", 1, 0, [], []],
			["a3", 2, null, [], [0]],
		],
	);
});

test("两层分支：竖线按段首的末位关系延续", async () => {
	const inside = link(assistant("a2", "u2", "A1"), user("u4", "a2", "A1a"), user("u5", "a2", "A1b"));
	const tree = [
		link(
			user("u1", null, "开场"),
			link(
				assistant("a1", "u1", "收到"),
				link(user("u2", "a1", "分支 A"), inside),
				link(user("u3", "a1", "分支 B"), assistant("a3", "u3", "B 的回答")),
			),
		),
	];
	await assertSameLayout(tree, "u4", "嵌套分支");
	assert.deepEqual(
		webLayout(tree, "u4").map((row) => [row.id, row.indent, row.square, row.lines, row.blanks]),
		[
			["u1", 0, null, [], []],
			["a1", 0, null, [], []],
			["u2", 1, 0, [], []],
			["a2", 2, null, [0], []],
			["u4", 3, 2, [0], []],
			["u5", 3, 2, [0], []],
			["u3", 1, 0, [], []],
			["a3", 2, null, [], [0]],
		],
	);
});

test("隐藏条目重挂到最近的可见祖先", async () => {
	const mc = modelChange("m1", "a1");
	const tree = [
		link(
			user("u1", null, "开场"),
			link(
				assistant("a1", "u1", "收到"),
				link(user("u2", "a1", "分支 A"), assistant("a2", "u2", "A 的回答")),
				link(mc, user("u3", "m1", "分支 B")),
			),
		),
	];
	await assertSameLayout(tree, "a2", "隐藏设置条目后");
});

test("无正文的 assistant 与工具结果", async () => {
	const tree = [
		link(
			user("u1", null, "开场"),
			link(assistant("a1", "u1", "收到"), link(toolResult("t1", "a1", "读取结果"), assistant("a2", "t1", ""))),
		),
	];
	await assertSameLayout(tree, "a2", "工具结果链");
	await assertSameLayout(tree, "t1", "工具结果链（叶在工具行）");
});

test("多根按虚拟根处理", async () => {
	const tree = [
		link(user("u1", null, "第一条链"), assistant("a1", "u1", "回答")),
		link(user("u2", null, "第二条链"), assistant("a2", "u2", "回答")),
	];
	await assertSameLayout(tree, "a2", "多根");
	// 虚拟根不注册竖线，多根时整棵树没有任何方块与竖线，只靠缩进区分层级。
	assert.deepEqual(
		webLayout(tree, "a2").map((row) => [row.id, row.indent, row.square, row.lines, row.blanks]),
		[
			["u2", 0, null, [], []],
			["a2", 1, null, [], []],
			["u1", 0, null, [], []],
			["a1", 1, null, [], []],
		],
	);
});

test("打印一棵 TUI 的树，便于人工核对", async () => {
	const tree = [
		link(
			user("u1", null, "开场"),
			link(
				assistant("a1", "u1", "收到"),
				link(user("u2", "a1", "分支 A"), assistant("a2", "u2", "A 的回答")),
				link(
					user("u3", "a1", "分支 B"),
					link(toolResult("t1", "u3", "读取结果"), assistant("a3", "t1", "B 的回答")),
				),
			),
		),
	];
	const tui = await tuiLayout(tree, "a2");
	assert.ok(tui.text.some((line: string) => line.includes("分支 A")));
	console.log(tui.text.filter((line: string) => line.trim()).join("\n"));
});

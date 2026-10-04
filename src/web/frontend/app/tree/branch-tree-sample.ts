import type { TreeEntry, TreeNode } from "./branch-tree";

/**
 * 设计预览与画布对照用的示例会话。
 * 结构：根到首个分支点是单子链条（无方块无竖线）；一个分支点有两个子条目，
 * 各自子树里再含压缩、分支总结、无正文助手与设置类条目。
 * 当前叶是「已按定值清单完成精修」那一行。
 */
export const sampleTree = (): { tree: TreeNode[]; leafId: string } => {
	const entries: TreeEntry[] = [];
	let clock = 0;
	const at = () => `2026-09-30T10:${String(clock++).padStart(2, "0")}:00Z`;
	const add = (entry: TreeEntry) => {
		entries.push(entry);
		return entry.id;
	};
	const user = (id: string, parentId: string | null, content: string) =>
		add({ id, parentId, type: "message", timestamp: at(), message: { role: "user", content } });
	const assistant = (id: string, parentId: string | null, text: string) =>
		add({
			id,
			parentId,
			type: "message",
			timestamp: at(),
			message: { role: "assistant", content: text, stopReason: "stop" },
		});
	const assistantTurn = (
		id: string,
		parentId: string | null,
		text: string,
		calls: { id: string; name: string; arguments: Record<string, unknown> }[],
	) =>
		add({
			id,
			parentId,
			type: "message",
			timestamp: at(),
			message: {
				role: "assistant",
				stopReason: "toolUse",
				content: [
					...(text ? [{ type: "text", text }] : []),
					...calls.map((call) => ({ type: "toolCall", ...call })),
				],
			},
		});
	const toolResult = (id: string, parentId: string, toolCallId: string, toolName: string, text: string) =>
		add({
			id,
			parentId,
			type: "message",
			timestamp: at(),
			message: { role: "toolResult", toolCallId, toolName, content: [{ type: "text", text }] },
		});

	const u1 = user("u1", null, "你好，我们进入到 checkpoint4 的 UI 精修环节，充分阅读本项目");
	const a1 = assistantTurn("a1", u1, "好的，我先用当前环境里的工具读一遍项目结构。", [
		{ id: "call-ls", name: "ls", arguments: { path: "." } },
	]);
	const t1 = toolResult("t1", a1, "call-ls", "ls", ".audit/ .git/ .gitattributes .github/ .gitignore …");
	const a2 = assistant("a2", t1, "项目结构已确认。接下来对照 R1/R2 缺陷清单逐条核对画布与实现。");
	const u2 = user("u2", a2, "注意，你在一个 pi 的环境中");
	const a3 = assistantTurn("a3", u2, "好的。我先列出 checkpoint4 的核对范围，再逐页检查。", [
		{ id: "call-fffind", name: "fffind", arguments: { pattern: "app-router" } },
		{ id: "call-read", name: "read", arguments: { path: "doc/web/SPEC/app-router.md" } },
	]);
	const t2 = toolResult(
		"t2",
		a3,
		"call-fffind",
		"fffind",
		"doc/web/SPEC/app-router.md doc/web/SPEC/frontend.md doc/web/SPEC/commands.md …",
	);
	const t3 = toolResult(
		"t3",
		t2,
		"call-read",
		"read",
		"# CP4 工具卡片缺陷清单（R1）画布：src/web/design/cst-pilot-tools.pen …",
	);
	const c1 = add({
		id: "c1",
		parentId: t3,
		type: "compaction",
		timestamp: at(),
		tokensBefore: 182000,
		summary: "已读项目结构",
	});
	const a4 = assistant("a4", c1, "核对范围确定。R1 画布与实现不一致的共 56 条。");
	const u3 = user("u3", a4, "看上去很多，整理一批没有争议的，可以直接修的议题");
	const a5 = assistant("a5", u3, "[可直接修] 从 56 条里能直接定值的共 48 条，剩 8 条卡在裁定。");

	// 第一个分支：u4 → a6 → u5 →（无正文助手）→ t4 → a7 → 分支总结
	const u4 = user("u4", a5, "整理 W19 的结论，罗列他们发现的问题");
	const a6 = assistant("a6", u4, "读完了。herdr 里 cst-pilot 的 pi 会话共 3 个。");
	const u5 = user("u5", a6, "对于这些可以直接修的议题，通过一个 html 文件展示");
	const blank = assistantTurn("blank", u5, "", [
		{ id: "call-pwsh", name: "powershell", arguments: { command: "node scripts/render-report.mjs" } },
	]);
	const t4 = toolResult("t4", blank, "call-pwsh", "powershell", "R1-light-error-00-disk-usage-1-0s.png 已生成");
	const a7 = assistant("a7", t4, "已按定值清单完成精修，共修改 5 个页面。");
	add({ id: "s1", parentId: a7, type: "branch_summary", timestamp: at(), summary: "已完成 R1/R2 缺陷核对与定值" });

	// 第二个分支：另一个用户条目，末位
	const u6 = user("u6", a5, "另外把结论追加到 doc/Notice.md");
	const a8 = assistant("a8", u6, "已写入 Notice.md 的缺陷清单小节。");
	// 隐藏条目：设置类与无正文助手，不占可见行
	add({ id: "h3", parentId: a8, type: "label", timestamp: at(), label: "待复核" });
	add({ id: "h4", parentId: a8, type: "model_change", timestamp: at(), modelId: "deepseek-v4-flash" });
	add({
		id: "h5",
		parentId: a8,
		type: "message",
		timestamp: at(),
		message: { role: "assistant", content: [], stopReason: "toolUse" },
	});

	const nodes = new Map<string, TreeNode>(entries.map((entry) => [entry.id, { entry, children: [] }]));
	const roots: TreeNode[] = [];
	for (const entry of entries) {
		const node = nodes.get(entry.id);
		if (!node) continue;
		const parent = entry.parentId ? nodes.get(entry.parentId) : undefined;
		if (entry.parentId === null || !parent) roots.push(node);
		else parent.children.push(node);
	}
	return { tree: roots, leafId: a7 };
};

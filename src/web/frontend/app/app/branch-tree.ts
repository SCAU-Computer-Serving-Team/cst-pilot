// 分支树的纯逻辑：可见性过滤、排序、层级与连接符/竖线的推导、折叠、行文案。
// 对齐 pi TUI 的 TreeList（dist/modes/interactive/components/tree-selector.js）：
//   flattenTree / recalculateVisualStructure 决定 indent、showConnector、gutters；
//   isFoldable 决定可折叠行。规则说明见 doc/web/SPEC/branch-tree.md。

export type TreeMessage = {
	role: string;
	content?: unknown;
	stopReason?: string;
	errorMessage?: string;
	toolCallId?: string;
	toolName?: string;
	command?: string;
};
export type TreeEntry = {
	id: string;
	parentId: string | null;
	type: string;
	timestamp: string;
	message?: TreeMessage;
	summary?: string;
	tokensBefore?: number;
	customType?: string;
	content?: unknown;
	name?: string;
	label?: string;
	modelId?: string;
	thinkingLevel?: string;
};
export type TreeNode = {
	entry: TreeEntry;
	children: TreeNode[];
	label?: string;
	labelTimestamp?: string;
};

const SETTINGS_TYPES = new Set(["label", "custom", "model_change", "thinking_level_change", "session_info"]);

export function textContent(content: unknown): string {
	if (typeof content === "string") return content;
	if (!Array.isArray(content)) return "";
	let result = "";
	for (const block of content) {
		if (typeof block === "object" && block !== null && (block as { type?: unknown }).type === "text") {
			result += (block as { text?: string }).text ?? "";
		}
	}
	return result;
}

/** 默认视图的可见性：隐藏设置类条目与无正文 assistant（错误、中止与当前叶除外）。 */
export function visibleByDefault(entry: TreeEntry, leafId: string | null): boolean {
	if (SETTINGS_TYPES.has(entry.type)) return false;
	if (entry.type === "message" && entry.message?.role === "assistant" && entry.id !== leafId) {
		const hasText = textContent(entry.message.content).trim().length > 0;
		const stop = entry.message.stopReason;
		const errorOrAborted = !!stop && stop !== "stop" && stop !== "toolUse";
		if (!hasText && !errorOrAborted) return false;
	}
	return true;
}

/** 过滤不可见条目；被隐藏条目的子条目挂到最近的可见祖先（TUI 的 findVisibleAncestor）。 */
export function buildVisibleTree(roots: TreeNode[], leafId: string | null): TreeNode[] {
	const attach = (node: TreeNode): TreeNode[] => {
		const children = node.children.flatMap(attach);
		return visibleByDefault(node.entry, leafId) ? [{ ...node, children }] : children;
	};
	return roots.flatMap(attach);
}

/** 一行的绘制参数，字段与 TUI 的 FlatNode 对应。 */
export type TreeRow = {
	node: TreeNode;
	/** TUI 的 indent：内容缩进级数，单子链条持平。 */
	indent: number;
	/** 实际显示的缩进；多根时整体左移一级。 */
	displayIndent: number;
	/** 该行画连接符。 */
	showConnector: boolean;
	/** 连接符所在列，等于 displayIndent - 1。 */
	connectorColumn: number;
	/** 需要延续的竖线；show 为假表示该列留空（末位兄弟的子树）。 */
	gutters: { column: number; show: boolean }[];
	isLast: boolean;
	/** 可见子条目数。 */
	childCount: number;
	/** 父行的可见子条目数；根为 0。 */
	parentChildCount: number;
	index: number;
	parentIndex: number;
	subtreeEnd: number;
};

const byTimestamp = (a: TreeNode, b: TreeNode) =>
	new Date(a.entry.timestamp).getTime() - new Date(b.entry.timestamp).getTime();

/**
 * 铺平可见树。排序：含当前叶的子树排最前，其余按时间戳。
 * 缩进：分支点的子条目 +1；段首的第一代再 +1；单子链条持平。
 */
export function layoutTree(roots: TreeNode[], leafId: string | null): TreeRow[] {
	const rows: TreeRow[] = [];
	if (roots.length === 0) return rows;
	const containsLeaf = new Map<TreeNode, boolean>();
	const mark = (node: TreeNode): boolean => {
		let has = node.entry.id === leafId;
		for (const child of node.children) if (mark(child)) has = true;
		containsLeaf.set(node, has);
		return has;
	};
	roots.forEach(mark);
	const order = (nodes: TreeNode[]) =>
		[...nodes].sort(
			(a, b) => Number(containsLeaf.get(b) ?? false) - Number(containsLeaf.get(a) ?? false) || byTimestamp(a, b),
		);

	type StackItem = {
		node: TreeNode;
		indent: number;
		justBranched: boolean;
		showConnector: boolean;
		isLast: boolean;
		gutters: { column: number; show: boolean }[];
		isVirtualRootChild: boolean;
		parentIndex: number;
		parentChildCount: number;
	};
	const multipleRoots = roots.length > 1;
	const orderedRoots = order(roots);
	const stack: StackItem[] = [];
	for (let i = orderedRoots.length - 1; i >= 0; i--) {
		stack.push({
			node: orderedRoots[i],
			indent: multipleRoots ? 1 : 0,
			justBranched: multipleRoots,
			showConnector: multipleRoots,
			isLast: i === orderedRoots.length - 1,
			gutters: [],
			isVirtualRootChild: multipleRoots,
			parentIndex: -1,
			parentChildCount: 0,
		});
	}
	while (stack.length > 0) {
		const item = stack.pop() as StackItem;
		const index = rows.length;
		const displayIndent = multipleRoots ? Math.max(0, item.indent - 1) : item.indent;
		const connectorDisplayed = item.showConnector && !item.isVirtualRootChild;
		const connectorColumn = Math.max(0, displayIndent - 1);
		const children = order(item.node.children);
		rows.push({
			node: item.node,
			indent: item.indent,
			displayIndent,
			showConnector: connectorDisplayed,
			connectorColumn,
			gutters: item.gutters,
			isLast: item.isLast,
			childCount: children.length,
			parentChildCount: item.parentChildCount,
			index,
			parentIndex: item.parentIndex,
			subtreeEnd: index,
		});
		const multipleChildren = children.length > 1;
		const childIndent = multipleChildren || (item.justBranched && item.indent > 0) ? item.indent + 1 : item.indent;
		const childGutters = connectorDisplayed
			? [...item.gutters, { column: connectorColumn, show: !item.isLast }]
			: item.gutters;
		for (let i = children.length - 1; i >= 0; i--) {
			stack.push({
				node: children[i],
				indent: childIndent,
				justBranched: multipleChildren,
				showConnector: multipleChildren,
				isLast: i === children.length - 1,
				gutters: childGutters,
				isVirtualRootChild: false,
				parentIndex: index,
				parentChildCount: children.length,
			});
		}
		// 子树占据的行区间：后代按 DFS 紧跟在后面。
		rows[index].subtreeEnd = index + countDescendants(item.node);
	}
	return rows;
}

/** 可见后代总数，用于回填行区间。 */
function countDescendants(node: TreeNode): number {
	let total = 0;
	for (const child of node.children) total += 1 + countDescendants(child);
	return total;
}

/** 行在某一列的绘制形态。 */
export type SlotMark = { kind: "empty" } | { kind: "square" } | { kind: "line" } | { kind: "line-hidden" };

/** 列 c 的形态：连接符列画方块；祖先段首传下来的竖线画线或留空。 */
export function slotMark(row: TreeRow, column: number): SlotMark {
	if (row.showConnector && column === row.connectorColumn) return { kind: "square" };
	const gutter = row.gutters.find((item) => item.column === column);
	if (gutter) return gutter.show ? { kind: "line" } : { kind: "line-hidden" };
	return { kind: "empty" };
}

/** 可折叠：有可见子条目，且（无可见父条目 或 可见父条目有多个可见子条目）。与 TUI isFoldable 一致。 */
export function foldable(row: TreeRow): boolean {
	return row.childCount > 0 && (row.parentIndex < 0 || row.parentChildCount > 1);
}

/** 应用折叠集合：被折叠行隐藏全部可见后代。返回剩余行与被折叠行的隐藏计数。 */
export function applyFold(
	rows: TreeRow[],
	folded: ReadonlySet<string>,
): { visible: TreeRow[]; hiddenCounts: Map<string, number> } {
	const hiddenCounts = new Map<string, number>();
	const ranges: (readonly [number, number])[] = [];
	for (const row of rows) {
		if (folded.has(row.node.entry.id) && foldable(row)) {
			hiddenCounts.set(row.node.entry.id, row.subtreeEnd - row.index);
			ranges.push([row.index, row.subtreeEnd] as const);
		}
	}
	if (!ranges.length) return { visible: rows, hiddenCounts };
	return {
		visible: rows.filter((row) => !ranges.some(([start, end]) => row.index > start && row.index <= end)),
		hiddenCounts,
	};
}

const shorten = (text: string, max = 200) =>
	text
		.replace(/[\n\t]/g, " ")
		.trim()
		.slice(0, max);

/** 工具调用的参数摘要（结果为空时作为回退）。 */
function toolSummary(args: Record<string, unknown>): string {
	const first = (keys: string[]) =>
		keys.map((key) => args[key]).find((value) => typeof value === "string" && value) as string | undefined;
	const main = first(["path", "file_path", "pattern", "command", "query", "url", "directory", "target"]);
	if (main)
		return `${main}`
			.replace(/[\n\t]/g, " ")
			.trim()
			.slice(0, 80);
	const json = JSON.stringify(args);
	return json.length > 40 ? `${json.slice(0, 40)}…` : json;
}

export type RowText = { kind: "user" | "assistant" | "tool" | "info"; role?: string; text: string };

/** 行文案：角色与摘要。kind 决定配色。 */
export function rowText(
	node: TreeNode,
	toolCalls: Map<string, { name: string; arguments: Record<string, unknown> }>,
): RowText {
	const entry = node.entry;
	if (entry.type === "message" && entry.message) {
		const message = entry.message;
		if (message.role === "user")
			return { kind: "user", role: "user:", text: shorten(textContent(message.content)) || "（图片消息）" };
		if (message.role === "assistant") {
			const text = shorten(textContent(message.content));
			if (text) return { kind: "assistant", role: "assistant:", text };
			if (message.stopReason === "aborted") return { kind: "assistant", role: "assistant:", text: "（已停止）" };
			if (message.errorMessage)
				return { kind: "assistant", role: "assistant:", text: shorten(message.errorMessage, 80) };
			return { kind: "assistant", role: "assistant:", text: "（无文本回复）" };
		}
		if (message.role === "toolResult") {
			const call = message.toolCallId ? toolCalls.get(message.toolCallId) : undefined;
			const name = call?.name ?? message.toolName ?? "tool";
			// 行内先给结果首行（便于辨认这一步返回了什么），结果为空时回退到参数摘要。
			const summary = shorten(textContent(message.content), 120) || (call ? toolSummary(call.arguments) : "");
			return { kind: "tool", text: `[${name}]${summary ? ` ${summary}` : ""}` };
		}
		if (message.role === "bashExecution")
			return { kind: "tool", text: `[bash] ${shorten(message.command ?? "", 80)}` };
		return { kind: "info", text: `[${message.role}]` };
	}
	if (entry.type === "custom_message")
		return { kind: "info", text: `[${entry.customType ?? "custom"}] ${shorten(textContent(entry.content))}` };
	if (entry.type === "compaction")
		return { kind: "info", text: `压缩 · ${Math.round((entry.tokensBefore ?? 0) / 1000)}k tokens` };
	if (entry.type === "branch_summary") return { kind: "info", text: `分支总结 · ${shorten(entry.summary ?? "")}` };
	if (entry.type === "session_info") return { kind: "info", text: `[标题: ${entry.name ?? "空"}]` };
	if (entry.type === "model_change") return { kind: "info", text: `[模型: ${entry.modelId ?? ""}]` };
	if (entry.type === "thinking_level_change")
		return { kind: "info", text: `[思考强度: ${entry.thinkingLevel ?? ""}]` };
	if (entry.type === "label") return { kind: "info", text: `[标记: ${entry.label ?? "(已清除)"}]` };
	return { kind: "info", text: `[${entry.type}]` };
}

/** 从 assistant 消息中提取工具调用，供工具结果行查名与参数。 */
export function collectToolCalls(roots: TreeNode[]): Map<string, { name: string; arguments: Record<string, unknown> }> {
	const map = new Map<string, { name: string; arguments: Record<string, unknown> }>();
	const walk = (node: TreeNode) => {
		const content = node.entry.message?.content;
		if (node.entry.type === "message" && node.entry.message?.role === "assistant" && Array.isArray(content)) {
			for (const block of content) {
				if (typeof block === "object" && block !== null && (block as { type?: unknown }).type === "toolCall") {
					const call = block as { id: string; name: string; arguments: Record<string, unknown> };
					map.set(call.id, { name: call.name, arguments: call.arguments });
				}
			}
		}
		node.children.forEach(walk);
	};
	roots.forEach(walk);
	return map;
}

/** 搜索：按行文案、标记与角色匹配全部查询词。 */
export function matchesSearch(node: TreeNode, text: RowText, query: string): boolean {
	const tokens = query.toLowerCase().split(/\s+/).filter(Boolean);
	if (!tokens.length) return true;
	const haystack = `${node.label ?? ""} ${text.role ?? ""} ${text.text}`.toLowerCase();
	return tokens.every((token) => haystack.includes(token));
}

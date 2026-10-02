// 逐词显现（transitions.dev 30-streaming-text）：拉丁字母与数字成词，其余字符（含中文）逐字成词，空白原样保留。
// 新词挂载时经 .t-stream-w 的 CSS 动画淡入；已存在的词节点不变，动画不重播。
const TOKEN = /[A-Za-z0-9]+(?:[.,:'’/+-][A-Za-z0-9]+)*|\s+|./gsu;
const isSpace = (token: string) => /^\s+$/.test(token);
const tokens = (text: string) => text.match(TOKEN) ?? [];

export function StreamWords({ text }: { text: string }) {
	return (
		<>
			{tokens(text).map((token, index) =>
				isSpace(token) ? (
					token
				) : (
					<span key={`${index}-${token}`} className="t-stream-w">
						{token}
					</span>
				),
			)}
		</>
	);
}

type HastNode = {
	type: string;
	value?: string;
	tagName?: string;
	properties?: Record<string, unknown>;
	children?: HastNode[];
};
const SKIP = new Set(["pre", "code", "script", "style"]);

/** rehype 插件：把 Markdown 文本节点逐词包进 .t-stream-w；代码块内部跳过，避免干扰高亮。 */
export function rehypeStreamWords() {
	const wrap = (nodes: HastNode[]): HastNode[] =>
		nodes.flatMap((node) => {
			if (node.type === "text" && node.value) {
				return tokens(node.value).map(
					(token): HastNode =>
						isSpace(token)
							? { type: "text", value: token }
							: {
									type: "element",
									tagName: "span",
									properties: { className: ["t-stream-w"] },
									children: [{ type: "text", value: token }],
								},
				);
			}
			if (node.type === "element" && node.children && !SKIP.has(node.tagName ?? ""))
				node.children = wrap(node.children);
			return [node];
		});
	return (tree: HastNode) => {
		if (tree.children) tree.children = wrap(tree.children);
	};
}

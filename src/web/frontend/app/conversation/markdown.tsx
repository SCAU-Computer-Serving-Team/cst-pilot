import { memo, useState } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import remarkGfm from "remark-gfm";
import { rehypeStreamWords } from "./stream-words";

const markdownRemarkPlugins = [remarkGfm];
const markdownRehypePlugins = [rehypeHighlight];
// 已开始流式显示的文本在定稿后保留逐词结构；历史文本无需逐字包装。
const markdownRehypeStreamPlugins = [rehypeHighlight, rehypeStreamWords];
const markdownRehypeLivePlugins = [rehypeStreamWords];
const markdownComponents: Components = {
	a: ({ children, ...props }) => (
		<a {...props} target="_blank" rel="noopener noreferrer">
			{children}
		</a>
	),
	img: () => null,
	table: ({ children }) => (
		// biome-ignore lint/a11y/noNoninteractiveTabindex: 滚动区域支持键盘横向浏览宽表格。
		<section className="markdown-table" tabIndex={0} aria-label="表格">
			<table>{children}</table>
		</section>
	),
};

/** 同一解析树按块序号更新，追加段落和定稿不搬移已读内容、不替换容器。 */
export const Markdown = memo(function Markdown({ text, live = false }: { text: string; live?: boolean }) {
	const [streamed, setStreamed] = useState(live);
	if (live && !streamed) setStreamed(true);
	const plugins = live ? markdownRehypeLivePlugins : streamed ? markdownRehypeStreamPlugins : markdownRehypePlugins;
	return (
		<div className="markdown" data-streaming={live || undefined}>
			<ReactMarkdown remarkPlugins={markdownRemarkPlugins} rehypePlugins={plugins} components={markdownComponents}>
				{text}
			</ReactMarkdown>
		</div>
	);
});

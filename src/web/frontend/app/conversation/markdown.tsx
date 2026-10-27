import { memo } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import remarkGfm from "remark-gfm";
import { rehypeStreamWords } from "./stream-words";

const markdownRemarkPlugins = [remarkGfm];
// 流式与定稿共用文本结构；高亮仅在定稿时处理代码块。
const markdownRehypePlugins = [rehypeHighlight, rehypeStreamWords];
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
	return (
		<div className="markdown" data-streaming={live || undefined}>
			<ReactMarkdown
				remarkPlugins={markdownRemarkPlugins}
				rehypePlugins={live ? markdownRehypeLivePlugins : markdownRehypePlugins}
				components={markdownComponents}
			>
				{text}
			</ReactMarkdown>
		</div>
	);
});

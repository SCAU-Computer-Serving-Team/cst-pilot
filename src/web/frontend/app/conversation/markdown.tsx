import { memo } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import remarkGfm from "remark-gfm";
import { splitStableText } from "./markdown-split";
import { rehypeStreamWords } from "./stream-words";

// 插件数组与组件映射提到模块级：react-markdown 按引用比较依赖，内联字面量会让每次渲染重建解析器、重新解析全文。
const markdownRemarkPlugins = [remarkGfm];
const markdownRehypePlugins = [rehypeHighlight];
// 流式期间跳过代码高亮：highlight.js 对代码块的重跑是单帧最贵成本，定稿后一次性高亮。
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
export const Markdown = memo(function Markdown({ text, live }: { text: string; live?: boolean }) {
	return (
		<div className="markdown">
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
// 流式正文分段：已定稿前缀 memo 缓存（含高亮），每帧只重解析尾段。
export const LiveMarkdown = memo(function LiveMarkdown({ text }: { text: string }) {
	const { stable, tail } = splitStableText(text);
	return (
		<div className="markdown">
			{stable && (
				<ReactMarkdown
					remarkPlugins={markdownRemarkPlugins}
					rehypePlugins={markdownRehypePlugins}
					components={markdownComponents}
				>
					{stable}
				</ReactMarkdown>
			)}
			<ReactMarkdown
				remarkPlugins={markdownRemarkPlugins}
				rehypePlugins={markdownRehypeLivePlugins}
				components={markdownComponents}
			>
				{tail}
			</ReactMarkdown>
		</div>
	);
});

import { memo, useMemo, useRef } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import remarkGfm from "remark-gfm";
import { CodeBlock } from "../shell/code-block";
import { splitStreamingMarkdownBlocks } from "./markdown-blocks";
import { rehypeStreamWords } from "./stream-words";

const markdownRemarkPlugins = [remarkGfm];
const markdownRehypePlugins = [rehypeHighlight];
const markdownRehypeLivePlugins = [rehypeStreamWords];
const markdownComponents: Components = {
	a: ({ children, ...props }) => (
		<a {...props} target="_blank" rel="noopener noreferrer">
			{children}
		</a>
	),
	img: () => null,
	pre: ({ children }) => <CodeBlock header>{children}</CodeBlock>,
	table: ({ children }) => (
		// biome-ignore lint/a11y/noNoninteractiveTabindex: 滚动区域支持键盘横向浏览宽表格。
		<section className="markdown-table" tabIndex={0} aria-label="表格">
			<table>{children}</table>
		</section>
	),
};

const StaticMarkdownBlock = memo(function StaticMarkdownBlock({ text }: { text: string }) {
	return (
		<ReactMarkdown
			remarkPlugins={markdownRemarkPlugins}
			rehypePlugins={markdownRehypePlugins}
			components={markdownComponents}
		>
			{text}
		</ReactMarkdown>
	);
});

const LiveMarkdownTail = memo(function LiveMarkdownTail({ text }: { text: string }) {
	return (
		<ReactMarkdown
			remarkPlugins={markdownRemarkPlugins}
			rehypePlugins={markdownRehypeLivePlugins}
			components={markdownComponents}
		>
			{text}
		</ReactMarkdown>
	);
});

/** 已定稿前缀块缓存复用不重排不重播，仅末尾活跃块增量逐词显现；历史消息单次解析。 */
export const Markdown = memo(function Markdown({ text, live = false }: { text: string; live?: boolean }) {
	const wasLiveRef = useRef(live);
	if (live) wasLiveRef.current = true;

	const split = useMemo(() => (live || wasLiveRef.current ? splitStreamingMarkdownBlocks(text) : null), [text, live]);

	if (!split) {
		return (
			<div className="markdown">
				<StaticMarkdownBlock text={text} />
			</div>
		);
	}

	return (
		<div className="markdown" data-streaming={live || undefined}>
			{split.settled.map((block, index) => (
				// biome-ignore lint/suspicious/noArrayIndexKey: 追加式定稿块按块序固定，前缀不变时 React 跳过重渲染。
				<StaticMarkdownBlock key={`b-${index}`} text={block} />
			))}
			{split.tail && (live ? <LiveMarkdownTail text={split.tail} /> : <StaticMarkdownBlock text={split.tail} />)}
		</div>
	);
});

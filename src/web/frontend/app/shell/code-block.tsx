import { Check, Copy } from "lucide-react";
import { Children, isValidElement, type ReactNode, useRef } from "react";
import { useCopyFeedback } from "./use-copy-feedback";

function detectLanguage(children: ReactNode): string {
	for (const child of Children.toArray(children)) {
		if (!isValidElement<{ className?: string }>(child)) continue;
		const match = /(?:^|\s)language-([\w#+-]+)(?:\s|$)/i.exec(child.props.className ?? "");
		if (match?.[1]) return match[1].toLowerCase();
	}
	return "code";
}

/** 只复制正文节点，保留缩进与换行；点击时读取当前流式内容。 */
export function CodeBlock({
	children,
	className = "",
	header = false,
	language,
}: {
	children: ReactNode;
	className?: string;
	/** Markdown 代码块启用 36px 顶栏（语言名 + 复制按钮，对齐 HmEJM / storybook.html）。 */
	header?: boolean;
	language?: string;
}) {
	const content = useRef<HTMLPreElement>(null);
	const { status, copy } = useCopyFeedback({ errorDurationMs: null });
	const feedback = status === "copied" ? "已复制" : status === "error" ? "复制失败，请手动选择复制或重试。" : "";
	const langLabel = language || (header ? detectLanguage(children) : "");
	const copyButton = (
		<button
			type="button"
			className={`btn-icon sm-28 code-copy-button${header ? " code-copy-button--inline" : ""}`}
			aria-label={status === "copied" ? "已复制代码" : "复制代码"}
			title={status === "copied" ? "已复制" : "复制代码"}
			data-copied={status === "copied" || undefined}
			onClick={() => void copy(content.current?.textContent ?? "")}
		>
			{status === "copied" ? <Check size={14} aria-hidden="true" /> : <Copy size={14} aria-hidden="true" />}
		</button>
	);
	return (
		<div className={`code-block${header ? " code-block--header" : ""}`}>
			{header && (
				<div className="code-block-header">
					<span className="code-block-lang">{langLabel}</span>
					{copyButton}
				</div>
			)}
			<pre ref={content} className={`code-block-content ${className}`}>
				{children}
			</pre>
			{!header && copyButton}
			<output className="code-copy-feedback" data-error={status === "error" || undefined}>
				{feedback}
			</output>
		</div>
	);
}

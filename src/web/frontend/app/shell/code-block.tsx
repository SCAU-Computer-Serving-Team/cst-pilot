import { Check, Copy } from "lucide-react";
import { type ReactNode, useEffect, useRef, useState } from "react";

/** 只复制正文节点，保留缩进与换行；点击时读取当前流式内容。 */
export function CodeBlock({ children, className = "" }: { children: ReactNode; className?: string }) {
	const content = useRef<HTMLPreElement>(null);
	const active = useRef(true);
	const copying = useRef(false);
	const timer = useRef<number | undefined>(undefined);
	const [status, setStatus] = useState<"idle" | "copied" | "error">("idle");
	useEffect(() => {
		active.current = true;
		return () => {
			active.current = false;
			window.clearTimeout(timer.current);
		};
	}, []);
	async function copy() {
		if (copying.current) return;
		copying.current = true;
		window.clearTimeout(timer.current);
		try {
			await navigator.clipboard.writeText(content.current?.textContent ?? "");
			if (!active.current) return;
			setStatus("copied");
			timer.current = window.setTimeout(() => setStatus("idle"), 2000);
		} catch {
			if (active.current) setStatus("error");
		} finally {
			copying.current = false;
		}
	}
	const feedback = status === "copied" ? "已复制" : status === "error" ? "复制失败，请手动选择复制或重试。" : "";
	return (
		<div className="code-block">
			<pre ref={content} className={`code-block-content ${className}`}>
				{children}
			</pre>
			<button
				type="button"
				className="code-copy-button"
				aria-label="复制代码"
				title={status === "copied" ? "已复制" : "复制代码"}
				data-copied={status === "copied" || undefined}
				onClick={() => void copy()}
			>
				{status === "copied" ? <Check size={16} aria-hidden="true" /> : <Copy size={16} aria-hidden="true" />}
			</button>
			<output className="code-copy-feedback" data-error={status === "error" || undefined}>
				{feedback}
			</output>
		</div>
	);
}

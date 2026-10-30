import type { ExportSnapshot } from "../../../../../agent/home/extensions/web/shared/exports";
import { mapTool } from "../cards/map-tool";
import { matchToolResults } from "../cards/match-results";
import { isSpecialTool, SpecialTool } from "../cards/special-tool";
import { ToolBlocks } from "../cards/tool-card";
import { Markdown } from "../conversation/markdown";
import { CodeBlock } from "../shell/code-block";

/** 无写入动作、分页或懒加载的会话呈现；复用聊天 Markdown、代码、工具块与主题变量。 */
export function ExportDocument({ snapshot }: { snapshot: ExportSnapshot }) {
	const results = matchToolResults(snapshot.entries);
	return (
		<main className="export-document">
			<header className="export-heading">
				<span>CST Pilot</span>
				<h1>{snapshot.title}</h1>
				<p>{new Date(snapshot.capturedAt).toLocaleString("zh-CN", { hour12: false })} · 会话快照</p>
			</header>
			<section className="messages-inner export-transcript" aria-label="导出对话">
				{snapshot.entries
					.filter(({ message }) => message.role !== "toolResult")
					.map((entry) => {
						const { message } = entry;
						const parts =
							typeof message.content === "string"
								? [{ type: "text" as const, text: message.content }]
								: message.content;
						return (
							<article
								className={`export-message ${message.role === "user" ? "user-line" : "assistant-block"}`}
								key={entry.id}
							>
								<div className={message.role === "user" ? "user-bubble" : "export-answer"}>
									{message.role === "branchSummary" && (
										<details open className="export-process">
											<summary>分支已总结</summary>
											<CodeBlock>{message.summary}</CodeBlock>
										</details>
									)}
									{parts.map((part, index) => {
										const key = `${entry.id}-${index}`;
										if (part.type === "text")
											return message.role === "user" ? (
												<span key={key}>{part.text}</span>
											) : (
												<Markdown key={key} text={part.text} />
											);
										if (part.type === "image")
											return ["image/png", "image/jpeg", "image/gif", "image/webp"].includes(
												part.mimeType,
											) ? (
												<img
													key={key}
													className="message-image"
													alt={`附图 ${index + 1}`}
													src={`data:${part.mimeType};base64,${part.data}`}
												/>
											) : null;
										if (part.type === "thinking")
											return (
												<details open className="export-process" key={key}>
													<summary>思考过程</summary>
													<CodeBlock className="tool-raw">{part.thinking}</CodeBlock>
												</details>
											);
										const result = results.get(entry.id)?.get(part.id);
										if (isSpecialTool(part) && part.name !== "web_search")
											return (
												<div className="export-tool" key={key}>
													<SpecialTool call={part} result={result} live={false} requests={[]} />
												</div>
											);
										return (
											<details open className="export-tool tool-card" key={key}>
												<summary>
													{part.name} · {result ? (result.isError ? "执行失败" : "已完成") : "结果未完成"}
												</summary>
												{result ? (
													<ToolBlocks blocks={mapTool(part, result).blocks} all />
												) : (
													<CodeBlock>{JSON.stringify(part.arguments, null, 2)}</CodeBlock>
												)}
											</details>
										);
									})}
									{message.errorMessage && <p className="message-warning">{message.errorMessage}</p>}
									{message.stopReason === "aborted" && <p className="message-warning">已停止生成</p>}
								</div>
							</article>
						);
					})}
			</section>
		</main>
	);
}

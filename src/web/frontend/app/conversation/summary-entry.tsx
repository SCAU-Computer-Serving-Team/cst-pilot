import { GitBranch, Minimize2 } from "lucide-react";
import { memo, useEffect, useState } from "react";
import { CodeBlock } from "../shell/code-block";
import type { TurnEntry } from "./conversation-parts";
import { TranscriptDisclosure, type TranscriptFormat } from "./transcript-disclosure";

/** 分支总结与上下文压缩消息共用折叠标题、时间和摘要布局。 */
export const SummaryEntry = memo(function SummaryEntry({
	entry,
	format,
	forceOpen = false,
}: {
	entry: TurnEntry;
	format?: TranscriptFormat;
	forceOpen?: boolean;
}) {
	const [open, setOpen] = useState(format === "png" || forceOpen);
	useEffect(() => {
		if (forceOpen) setOpen(true);
	}, [forceOpen]);
	const isCompaction = entry.message.role === "compactionSummary";
	const tokensBefore = entry.message.tokensBefore ?? 0;
	const tokensText =
		isCompaction && tokensBefore > 0
			? ` · 压缩前 ${tokensBefore >= 1000 ? `${Math.round(tokensBefore / 1000)}k` : tokensBefore} tokens`
			: "";
	return (
		<article className="assistant-block summary-block">
			<TranscriptDisclosure
				format={format}
				isExpanded={open}
				onExpandedChange={setOpen}
				triggerClassName="summary-head"
				heading={
					<>
						{isCompaction ? (
							<Minimize2 size={18} aria-hidden="true" />
						) : (
							<GitBranch size={18} aria-hidden="true" />
						)}
						<span className="summary-title">{isCompaction ? `上下文已压缩${tokensText}` : "分支已总结"}</span>
					</>
				}
				suffix={
					<span className="summary-time">
						{new Date(entry.message.timestamp).toLocaleTimeString("zh-CN", {
							hour: "2-digit",
							minute: "2-digit",
						})}
					</span>
				}
			>
				<section className="tool-text">
					<CodeBlock className="tool-raw">{entry.message.summary ?? ""}</CodeBlock>
				</section>
			</TranscriptDisclosure>
		</article>
	);
});

import { GitBranch } from "lucide-react";
import { memo, useState } from "react";
import { CodeBlock } from "../shell/code-block";
import type { TurnEntry } from "./conversation-parts";
import { TranscriptDisclosure, type TranscriptFormat } from "./transcript-disclosure";

/** 分支总结与聊天、导出共用标题、时间和原文布局。 */
export const SummaryEntry = memo(function SummaryEntry({
	entry,
	format,
}: {
	entry: TurnEntry;
	format?: TranscriptFormat;
}) {
	const [open, setOpen] = useState(format === "png");
	return (
		<article className="assistant-block summary-block">
			<TranscriptDisclosure
				format={format}
				isExpanded={open}
				onExpandedChange={setOpen}
				triggerClassName="summary-head"
				heading={
					<>
						<GitBranch size={18} aria-hidden="true" />
						<span className="summary-title">分支已总结</span>
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

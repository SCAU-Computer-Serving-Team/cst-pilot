import { matchToolResults } from "../cards/match-results";
import { AssistantContent } from "../conversation/assistant-content";
import { groupTurns } from "../conversation/conversation-parts";
import { SummaryEntry } from "../conversation/summary-entry";
import { UserMessage } from "../conversation/user-message";
import type { ExportSnapshot } from "../data/types";

/** 导出与聊天共用轮次、过程、正文和附图；只调整文档宽度、初始折叠层级与动作。 */
export function ExportDocument({ snapshot, preview = false }: { snapshot: ExportSnapshot; preview?: boolean }) {
	const results = matchToolResults(snapshot.entries);
	const turns = groupTurns(snapshot.entries.filter(({ message }) => message.role !== "toolResult"));
	const format = snapshot.options.format;
	return (
		<main className="export-document" data-format={format} data-preview={preview || undefined}>
			<header className="export-heading">
				<span>CST Pilot</span>
				<h1>{snapshot.title}</h1>
				<p>{new Date(snapshot.capturedAt).toLocaleString("zh-CN", { hour12: false })} · 会话快照</p>
			</header>
			<section className="messages-inner export-transcript" aria-label="导出对话">
				{turns.map((turn, index) => {
					const key = turn.user?.id ?? turn.summary?.id ?? turn.assistants[0]?.id ?? index;
					if (turn.summary) return <SummaryEntry key={key} entry={turn.summary} format={format} />;
					return (
						<div className="turn export-message" key={key}>
							{turn.user && <UserMessage message={turn.user.message} format={format} />}
							{turn.assistants.length > 0 && (
								<AssistantContent turn={turn} resultsByTurn={results} requests={[]} format={format} />
							)}
						</div>
					);
				})}
			</section>
		</main>
	);
}

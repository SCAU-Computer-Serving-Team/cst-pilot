import type { ReactNode } from "react";
import { SpecialTool } from "../cards/special-tool";
import type { PendingBitLocker } from "../data/bitlocker-state";
import type { Message } from "../data/web-state";
import { ErrorNotice } from "../shell/error-notice";
import { splitTurn, type Turn } from "./conversation-parts";
import { Markdown } from "./markdown";
import { MessageImage } from "./message-image";
import { ProcessSegment } from "./process-segment";
import type { TranscriptFormat } from "./transcript-disclosure";
import { processSegmentLabel, turnSegments } from "./turn-segments";

/** 聊天和导出共用整轮结构，间距、过程层级和引导线始终由同一组组件负责。 */
export function AssistantContent({
	id,
	turn,
	live = false,
	resultsByTurn,
	requests,
	firstProcessLabel = "已完成",
	format,
	children,
}: {
	id?: string;
	turn: Turn;
	live?: boolean;
	resultsByTurn: Map<string, Map<string, Message>>;
	requests: PendingBitLocker[];
	firstProcessLabel?: string;
	format?: TranscriptFormat;
	children?: ReactNode;
}) {
	const { process, finalText } = splitTurn(turn.assistants);
	const last = turn.assistants.at(-1);
	return (
		<article className="assistant-block assistant-turn" data-live={live || undefined}>
			{turnSegments(process).map((segment, index) => {
				if (segment.kind === "visible")
					return (
						<div className="turn-visible" key={segment.key}>
							{segment.parts.map(({ entry, final, part }, partIndex) =>
								part.kind === "special" ? (
									<SpecialTool
										key={`${entry.message.timestamp}-${part.call.id}`}
										call={part.call}
										sessionId={id}
										entryId={entry.id}
										requests={requests}
										result={resultsByTurn
											.get(entry.id ?? `stamp:${entry.message.timestamp}`)
											?.get(part.call.id)}
										live={live && final}
										format={format}
									/>
								) : part.kind === "image" ? (
									<MessageImage key={`${entry.id}-image-${partIndex}`} part={part.part} format={format} />
								) : null,
							)}
						</div>
					);
				const label = index === 0 ? firstProcessLabel : processSegmentLabel(segment.parts, resultsByTurn, live);
				return label ? (
					<ProcessSegment
						key={segment.key}
						parts={segment.parts}
						live={live}
						label={label}
						waiting={live && !process.length && !finalText}
						results={resultsByTurn}
						format={format}
					/>
				) : null;
			})}
			{finalText && <Markdown text={finalText} live={live} />}
			{(last?.message.stopReason === "error" || last?.message.errorMessage) && (
				<ErrorNotice model message={last.message.errorMessage || "模型调用失败，请检查登录状态和模型设置。"} />
			)}
			{last?.message.stopReason === "aborted" && <p className="message-warning">已停止生成</p>}
			{children}
		</article>
	);
}

import { useState } from "react";
import type { OutgoingMessage } from "../data/message-outbox";
import { messageOutbox } from "../data/use-message-outbox";
import type { Message } from "../data/web-state";
import { textOf } from "./conversation-parts";
import { MessageImage } from "./message-image";
import { OutgoingImages, OutgoingStatus } from "./outgoing-message";
import type { TranscriptFormat } from "./transcript-disclosure";

/** 乐观气泡与正式气泡共享组件和 key；确认只更新内容，不重新播放入场。 */
export function UserMessage({
	message,
	outgoing,
	fresh = false,
	format,
}: {
	message: Message;
	outgoing?: OutgoingMessage;
	fresh?: boolean;
	format?: TranscriptFormat;
}) {
	const [entering, setEntering] = useState(() =>
		format ? false : message.clientMessageId ? messageOutbox().claimEntrance(message.clientMessageId) : fresh,
	);
	return (
		<div
			className="user-line"
			data-fresh={entering || undefined}
			data-message-id={message.clientMessageId}
			onAnimationEnd={() => setEntering(false)}
		>
			<div className="user-message-content">
				<div className="user-bubble">
					{textOf(message)}
					{outgoing ? (
						<OutgoingImages hashes={outgoing.images} />
					) : (
						Array.isArray(message.content) &&
						message.content
							.filter((part) => part.type === "image")
							.map((part, index) => (
								<MessageImage key={`${index}-${part.mimeType}`} part={part} index={index} format={format} />
							))
					)}
				</div>
				{outgoing && <OutgoingStatus item={outgoing} />}
			</div>
		</div>
	);
}

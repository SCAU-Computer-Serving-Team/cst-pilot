import type { Message } from "../data/web-state";
import { textOf } from "./conversation-parts";
import { MessageImage } from "./message-image";
import type { TranscriptFormat } from "./transcript-disclosure";

/** 聊天与导出共用提问气泡和附图布局，导出移除图片预览操作。 */
export function UserMessage({ message, fresh = false, format }: { message: Message; fresh?: boolean; format?: TranscriptFormat }) {
	return (
		<div className="user-line" data-fresh={fresh || undefined}>
			<div className="user-bubble">
				{textOf(message)}
				{Array.isArray(message.content) && message.content.filter((part) => part.type === "image").map((part, index) => (
					<MessageImage key={`${index}-${part.mimeType}`} part={part} index={index} format={format} />
				))}
			</div>
		</div>
	);
}

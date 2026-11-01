import type { ContentPart } from "../data/web-state";
import { PreviewableImage } from "../images/image-preview";
import type { TranscriptFormat } from "./transcript-disclosure";

/** 对话附图共用尺寸与格式校验；导出保留图片并移除放大操作。 */
export function MessageImage({
	part,
	index = 0,
	format,
}: {
	part: Extract<ContentPart, { type: "image" }>;
	index?: number;
	format?: TranscriptFormat;
}) {
	if (!["image/png", "image/jpeg", "image/gif", "image/webp"].includes(part.mimeType)) return null;
	const src = `data:${part.mimeType};base64,${part.data}`;
	const alt = `附图 ${index + 1}`;
	return format ? (
		<img className="message-image" src={src} alt={alt} />
	) : (
		<PreviewableImage className="message-image" src={src} alt={alt} />
	);
}

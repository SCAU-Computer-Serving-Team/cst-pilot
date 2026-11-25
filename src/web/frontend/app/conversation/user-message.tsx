import { Button, Tooltip } from "@heroui/react";
import { Check, Copy, Pencil, RotateCcw } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { OutgoingMessage } from "../data/message-outbox";
import { messageOutbox } from "../data/use-message-outbox";
import type { ContentPart, Message } from "../data/web-state";
import { handleTextFieldVerticalArrow } from "../shell/input-shortcuts";
import { useCopyFeedback } from "../shell/use-copy-feedback";
import { useWorkspaceShell } from "../shell/workspace-shell-context";
import { textOf } from "./conversation-parts";
import { MessageImage } from "./message-image";
import { OutgoingImages, OutgoingStatus } from "./outgoing-message";
import type { TranscriptFormat } from "./transcript-disclosure";

export type UserImagePart = Extract<ContentPart, { type: "image" }>;

/** 乐观气泡与正式气泡共享组件和 key；确认只更新内容，不重新播放入场。 */
export function UserMessage({
	entryId,
	message,
	outgoing,
	fresh = false,
	running = false,
	format,
	onRetryTurn,
	onNotice,
	onError,
}: {
	entryId?: string;
	message: Message;
	outgoing?: OutgoingMessage;
	fresh?: boolean;
	running?: boolean;
	format?: TranscriptFormat;
	onRetryTurn?: (entryId: string, text: string, images: UserImagePart[]) => Promise<boolean>;
	onNotice?: (message: string) => void;
	onError?: (message: string) => void;
}) {
	const [entering, setEntering] = useState(() =>
		format ? false : message.clientMessageId ? messageOutbox().claimEntrance(message.clientMessageId) : fresh,
	);
	const { copied, copy } = useCopyFeedback({
		onCopied: () => onNotice?.("已复制消息内容"),
		onError: () => onError?.("无法复制，请检查浏览器权限。"),
	});
	const [editing, setEditing] = useState(false);
	const [submitting, setSubmitting] = useState(false);
	const [draft, setDraft] = useState("");
	const editButtonRef = useRef<HTMLButtonElement>(null);
	const editInputRef = useRef<HTMLTextAreaElement>(null);
	const mounted = useRef(true);
	const shell = useWorkspaceShell();
	const tooltipsDisabled = Boolean(shell?.appActive);
	useEffect(() => {
		mounted.current = true;
		return () => {
			mounted.current = false;
		};
	}, []);
	useEffect(() => {
		if (!editing) return;
		const el = editInputRef.current;
		if (!el) return;
		el.focus();
		el.setSelectionRange(el.value.length, el.value.length);
	}, [editing]);
	const originalText = textOf(message);
	const userImages: UserImagePart[] = Array.isArray(message.content)
		? message.content.filter((part): part is UserImagePart => part.type === "image")
		: [];
	const canMutate = !running && !submitting && !!entryId && !!onRetryTurn;
	const canRetry = canMutate && (Boolean(originalText.trim()) || userImages.length > 0);
	const canSubmitEdit = canMutate && (Boolean(draft.trim()) || userImages.length > 0);
	function cancelEdit() {
		if (submitting) return;
		setEditing(false);
		requestAnimationFrame(() => editButtonRef.current?.focus({ preventScroll: true }));
	}
	async function submitEdit() {
		if (!entryId || !onRetryTurn || !canSubmitEdit) return;
		if (draft === originalText) {
			cancelEdit();
			return;
		}
		setSubmitting(true);
		try {
			const ok = await onRetryTurn(entryId, draft, userImages);
			if (mounted.current && ok) setEditing(false);
		} finally {
			if (mounted.current) setSubmitting(false);
		}
	}
	return (
		<div
			className="user-line"
			data-fresh={entering || undefined}
			data-message-id={message.clientMessageId}
			onAnimationEnd={() => setEntering(false)}
		>
			<div className="user-message-content">
				{editing ? (
					<div className="user-edit-card">
						<textarea
							ref={editInputRef}
							className="user-edit-input"
							aria-label="修改消息内容"
							rows={Math.min(10, Math.max(2, draft.split("\n").length))}
							disabled={submitting}
							value={draft}
							onChange={(event) => setDraft(event.target.value)}
							onKeyDown={(event) => {
								if (event.nativeEvent.isComposing || event.keyCode === 229) return;
								if (event.key === "Escape") {
									event.preventDefault();
									cancelEdit();
									return;
								}
								if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
									event.preventDefault();
									void submitEdit();
									return;
								}
								handleTextFieldVerticalArrow(event);
							}}
						/>
						<div className="user-edit-actions">
							<button
								type="button"
								className="tier-1-btn-outline user-edit-btn"
								disabled={submitting}
								onClick={cancelEdit}
							>
								取消
							</button>
							<button
								type="button"
								className="tier-1-btn-primary user-edit-btn"
								disabled={!canSubmitEdit}
								onClick={() => void submitEdit()}
							>
								{submitting ? "正在发送…" : "保存并重新发送"}
							</button>
						</div>
					</div>
				) : (
					<div className="user-bubble">
						{originalText}
						{outgoing ? (
							<OutgoingImages hashes={outgoing.images} />
						) : (
							userImages.map((part, index) => (
								<MessageImage key={`${index}-${part.mimeType}`} part={part} index={index} format={format} />
							))
						)}
					</div>
				)}
				{!format && !outgoing && !editing && (
					<div className="user-bubble-actions">
						<Tooltip delay={80} closeDelay={0} isDisabled={tooltipsDisabled}>
							<Button
								variant="ghost"
								isIconOnly
								className="btn-icon xs-24"
								data-copied={copied || undefined}
								aria-label={copied ? "已复制消息" : "复制消息"}
								onPress={() => void copy(originalText)}
							>
								{copied ? <Check size={14} aria-hidden="true" /> : <Copy size={14} aria-hidden="true" />}
							</Button>
							<Tooltip.Content className="app-tooltip" placement="top">
								{copied ? "已复制" : "复制"}
							</Tooltip.Content>
						</Tooltip>
						<Tooltip delay={80} closeDelay={0} isDisabled={tooltipsDisabled}>
							<Button
								ref={editButtonRef}
								variant="ghost"
								isIconOnly
								className="btn-icon xs-24"
								isDisabled={!canMutate}
								aria-label="修改消息"
								onPress={() => {
									setDraft(originalText);
									setEditing(true);
								}}
							>
								<Pencil size={14} aria-hidden="true" />
							</Button>
							<Tooltip.Content className="app-tooltip" placement="top">
								修改消息
							</Tooltip.Content>
						</Tooltip>
						<Tooltip delay={80} closeDelay={0} isDisabled={tooltipsDisabled}>
							<Button
								variant="ghost"
								isIconOnly
								className="btn-icon xs-24"
								isDisabled={!canRetry}
								aria-label="重新生成"
								onPress={() => {
									if (entryId && onRetryTurn) void onRetryTurn(entryId, originalText, userImages);
								}}
							>
								<RotateCcw size={14} aria-hidden="true" />
							</Button>
							<Tooltip.Content className="app-tooltip" placement="top">
								重新生成
							</Tooltip.Content>
						</Tooltip>
					</div>
				)}
				{outgoing && <OutgoingStatus item={outgoing} />}
			</div>
		</div>
	);
}

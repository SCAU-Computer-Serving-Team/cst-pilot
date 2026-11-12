import { Button } from "@heroui/react";
import { CircleAlert, Clock3 } from "lucide-react";
import { useEffect, useState } from "react";
import { Link } from "react-router";
import { type DraftImage, loadImages } from "../composer/draft-images";
import { presentError } from "../data/error-presenter";
import type { OutgoingMessage } from "../data/message-outbox";
import { messageOutbox } from "../data/use-message-outbox";
import { PreviewableImage } from "../images/image-preview";
import { ConfirmDialog } from "../shell/app-dialogs";

export const EDIT_OUTGOING_EVENT = "cst-edit-outgoing";
const labels = {
	waiting: "等待提交",
	sending: "发送中…",
	unknown: "发送结果待确认",
	failed: "发送失败",
	accepted: "已接受，正在同步…",
};
export function OutgoingImages({ hashes }: { hashes: string[] }) {
	const [images, setImages] = useState<DraftImage[]>([]);
	useEffect(() => {
		let active = true;
		let loaded: DraftImage[] = [];
		void loadImages(hashes)
			.then((result) => {
				loaded = result;
				if (active) setImages(result);
				else for (const image of result) URL.revokeObjectURL(image.url);
			})
			.catch(() => {});
		return () => {
			active = false;
			for (const image of loaded) URL.revokeObjectURL(image.url);
		};
	}, [hashes]);
	return images.map((image, index) => (
		<PreviewableImage key={image.hash} className="message-image" src={image.url} alt={`附图 ${index + 1}`} />
	));
}
export function OutgoingStatus({ item }: { item: OutgoingMessage }) {
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState("");
	const [confirmRemove, setConfirmRemove] = useState(false);
	const outbox = messageOutbox();
	async function act(action: () => Promise<void> | void) {
		if (busy) return;
		setBusy(true);
		try {
			await action();
			setError("");
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "操作未完成，请重试");
		} finally {
			setBusy(false);
		}
	}
	if (item.state === "sending" || item.state === "accepted") return null;
	const editable = item.state === "failed" || (item.state === "waiting" && !item.attempted);
	const uncertain = item.state === "unknown";
	return (
		<div className="outgoing-status" data-state={item.state}>
			<div className="outgoing-actions">
				<output>
					{item.state === "failed" || uncertain ? (
						<CircleAlert size={14} aria-hidden="true" />
					) : (
						<Clock3 size={14} aria-hidden="true" />
					)}
					{labels[item.state]}
				</output>
				{uncertain && (
					<Button variant="ghost" isDisabled={busy} onPress={() => void act(() => outbox.check(item.id))}>
						核对状态
					</Button>
				)}
				{(item.state === "failed" || uncertain) && (
					<Button variant="ghost" isDisabled={busy} onPress={() => void act(() => outbox.retry(item.id))}>
						重试
					</Button>
				)}
				{editable && (
					<Button
						variant="ghost"
						isDisabled={busy}
						onPress={() => window.dispatchEvent(new CustomEvent(EDIT_OUTGOING_EVENT, { detail: item.id }))}
					>
						编辑
					</Button>
				)}
				{(editable || uncertain) && (
					<Button
						variant="ghost"
						isDisabled={busy}
						onPress={() => {
							if (uncertain) {
								setConfirmRemove(true);
								return;
							}
							void act(() => outbox.remove(item.id));
						}}
					>
						移除
					</Button>
				)}
				{item.state === "failed" && /凭据|API.?key|所选模型/i.test(item.error ?? "") && (
					<Link to="/settings">配置模型</Link>
				)}
				{item.fromHome && item.sessionId && <Link to={`/s/${item.sessionId}`}>打开会话</Link>}
			</div>
			{(() => {
				const raw = error || item.error;
				if (!raw) return null;
				const formatted = presentError(raw, { input: true });
				return (
					<p role="alert">
						{formatted.description ? `${formatted.title}：${formatted.description}` : formatted.title}
					</p>
				);
			})()}
			{uncertain && <p>核对完成前请原样重试，避免重复发送。</p>}
			{confirmRemove && (
				<ConfirmDialog
					title="移除待确认消息？"
					description="发送结果尚未确认。移除本地记录无法撤回已到达后端的消息，后续可能仍出现回复。"
					confirmLabel="移除"
					danger
					busy={busy}
					onCancel={() => setConfirmRemove(false)}
					onConfirm={() => {
						setConfirmRemove(false);
						void act(() => outbox.remove(item.id));
					}}
				/>
			)}
		</div>
	);
}

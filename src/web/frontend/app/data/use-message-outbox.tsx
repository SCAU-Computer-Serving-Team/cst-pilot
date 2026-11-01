import { useEffect, useRef, useSyncExternalStore } from "react";
import { useLocation, useNavigate } from "react-router";
import { collectDraftImages, encodeImages, loadImages } from "../composer/draft-images";
import { draftKey } from "../composer/drafts";
import { ApiFailure, apiJson, sessionPath } from "./api";
import { MessageOutbox, type OutgoingMessage, parseMessageReceipt } from "./message-outbox";
import { useGlobalEvents } from "./web-state";

class SendPreparationError extends Error {}
let instance: MessageOutbox | undefined;
const signal = () => AbortSignal.timeout(15_000);
async function receipt(sessionId: string, id: string) {
	const result = await apiJson<{ item: unknown }>(sessionPath(sessionId, `/messages/${encodeURIComponent(id)}`), {
		signal: signal(),
	});
	if (!result || !Object.hasOwn(result, "item")) throw new Error("无法读取消息回执");
	return result.item === null ? null : parseMessageReceipt(result.item);
}
export function messageOutbox() {
	if (instance) return instance;
	instance = new MessageOutbox(
		sessionStorage,
		{
			async create(item) {
				const result = await apiJson<{ id: string }>("/api/sessions", {
					method: "POST",
					signal: signal(),
					idempotencyKey: item.id,
					body: item.provider
						? { provider: item.provider, modelId: item.modelId, thinkingLevel: item.thinkingLevel }
						: {},
				});
				if (!result || typeof result.id !== "string") throw new Error("新会话创建结果待确认");
				return result.id;
			},
			async send(item) {
				if (!item.sessionId) throw new Error("会话尚未确认");
				const images = await loadImages(item.images);
				try {
					if (images.length !== item.images.length)
						throw new SendPreparationError("待发送图片不完整，请恢复图片后重新提交。");
					const result = await apiJson<{ item: unknown }>(sessionPath(item.sessionId, "/messages"), {
						method: "POST",
						signal: signal(),
						idempotencyKey: item.id,
						body: {
							id: item.id,
							text: item.text,
							delivery: "queue",
							images: await encodeImages(images),
							...(item.skill ? { skill: item.skill } : {}),
						},
					});
					const accepted = parseMessageReceipt(result?.item);
					if (accepted.id !== item.id) throw new Error("发送回执无效，请核对发送结果。");
					return accepted;
				} finally {
					for (const image of images) URL.revokeObjectURL(image.url);
				}
			},
			async lookup(item) {
				if (item.sessionId) return { item: await receipt(item.sessionId, item.id) };
				const creation = await apiJson<{ status: "missing" | "started" | "done"; sessionId?: string }>(
					`/api/sessions/creation/${encodeURIComponent(item.id)}`,
					{ signal: signal() },
				);
				if (creation.status !== "done" || !creation.sessionId)
					return { creationPending: creation.status === "started" };
				return { sessionId: creation.sessionId, item: await receipt(creation.sessionId, item.id) };
			},
			isRejection: (error) =>
				error instanceof SendPreparationError ||
				(error instanceof ApiFailure &&
					([400, 401, 403, 404, 413, 422, 429].includes(error.status) || error.code === "credentials_required")),
		},
		() => {
			void collectDraftImages().catch(() => {});
		},
	);
	// 页面在保存发送记录后中断时，移除同 ID 的原草稿副本，保留后续新草稿。
	for (const item of instance.getSnapshot()) {
		const key = draftKey(item.fromHome ? undefined : item.sessionId);
		if (sessionStorage.getItem(`${key}:id`) !== item.id || sessionStorage.getItem(key) !== item.serialized) continue;
		try {
			sessionStorage.removeItem(key);
			sessionStorage.removeItem(`${key}:images`);
			sessionStorage.removeItem(`${key}:id`);
		} catch {
			/* 待发送记录仍持有完整内容。 */
		}
	}
	return instance;
}
export function useMessageOutbox() {
	const outbox = messageOutbox();
	const records = useSyncExternalStore(outbox.subscribe, outbox.getSnapshot, outbox.getSnapshot);
	return { outbox, records };
}

/** 路由之外持续管理核对与首页接管；恢复连接不会重发写请求。 */
export function MessageOutboxRecovery() {
	const { outbox, records } = useMessageOutbox();
	const location = useLocation();
	const navigate = useNavigate();
	const navigated = useRef(new Set(records.filter((item) => item.sessionId).map((item) => item.id)));
	useEffect(() => {
		void outbox.checkAll();
		const check = () => {
			void outbox.checkAll();
		};
		window.addEventListener("online", check);
		window.addEventListener("focus", check);
		return () => {
			window.removeEventListener("online", check);
			window.removeEventListener("focus", check);
		};
	}, [outbox]);
	useGlobalEvents((name) => {
		if (name === "open" || name === "reset" || name === "state") void outbox.checkAll();
	});
	useEffect(() => {
		const created = records.filter((item) => item.fromHome && item.sessionId && !navigated.current.has(item.id));
		for (const item of created) navigated.current.add(item.id);
		const item = created[0];
		if (
			location.pathname !== "/" ||
			!item ||
			(sessionStorage.getItem(draftKey()) ?? "").trim() ||
			(sessionStorage.getItem(`${draftKey()}:images`) ?? "[]") !== "[]"
		)
			return;
		navigated.current.add(item.id);
		navigate(`/s/${item.sessionId}`, { viewTransition: true });
	}, [records, navigate, location.pathname]);
	return outbox.recoveryError ? (
		<p role="alert" className="connection-banner">
			{outbox.recoveryError}
		</p>
	) : null;
}

export function outgoingFor(records: readonly OutgoingMessage[], sessionId?: string) {
	return records.filter((item) => (sessionId ? item.sessionId === sessionId : item.fromHome));
}

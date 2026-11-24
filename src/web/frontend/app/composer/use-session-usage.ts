import { useCallback, useEffect, useRef, useState } from "react";
import { apiJson } from "../data/api";
import { subscribeGlobalEvents, subscribeSessionEvents } from "../data/event-stream";

export interface SessionContextUsage {
	tokens: number | null;
	contextWindow: number;
	percent: number | null;
}

export interface SessionUsageTokens {
	input: number;
	output: number;
	cacheRead: number;
	cacheWrite: number;
	total: number;
}

export interface SessionUsageSummary {
	scope?: string;
	sourceEntryId?: string | null;
	tokens: SessionUsageTokens;
	cacheHitRate: number | null;
}

export interface SessionUsagePayload {
	sessionId: string;
	leafId: string | null;
	contextUsage: SessionContextUsage | null;
	usage: SessionUsageSummary | null;
}

/** 主动拉取当前会话用量。订阅与未完成的请求随会话或启用状态一起销毁。 */
export function useSessionUsage(sessionId: string | undefined, enabled: boolean) {
	const [payload, setPayload] = useState<SessionUsagePayload | null>(null);
	const subscription = useRef<{ id: string; refresh: () => void } | null>(null);

	useEffect(() => {
		setPayload(null);
		if (!sessionId || !enabled) return;
		let controller: AbortController | undefined;
		let timer: number | undefined;
		const refresh = () => {
			window.clearTimeout(timer);
			timer = undefined;
			controller?.abort();
			const request = new AbortController();
			controller = request;
			void apiJson<SessionUsagePayload>(`/api/sessions/${encodeURIComponent(sessionId)}/usage`, {
				signal: request.signal,
			})
				.then((data) => {
					if (!request.signal.aborted && data.sessionId === sessionId) setPayload(data);
				})
				.catch(() => {
					/* 保留当前读数，等待下次触发或重连。 */
				});
		};
		const schedule = () => {
			timer ??= window.setTimeout(refresh, 60);
		};
		subscription.current = { id: sessionId, refresh };
		refresh();
		const stopGlobal = subscribeGlobalEvents((name, event) => {
			if (name === "open" || name === "reset") {
				schedule();
				return;
			}
			if (name !== "state") return;
			try {
				const change = JSON.parse(event?.data ?? "null");
				if (change?.type === "usage_changed" && change.sessionId === sessionId) schedule();
			} catch {
				/* 忽略格式异常的全局事件。 */
			}
		});
		const session = subscribeSessionEvents(sessionId, (name, data) => {
			if (name === "open" || name === "reset") {
				schedule();
				return;
			}
			if (name !== "session") return;
			try {
				const change = JSON.parse(data ?? "null");
				if (
					change?.type === "compaction_end" ||
					change?.type === "session_tree" ||
					(change?.type === "message_end" && change.message?.role === "assistant") ||
					change?.type === "turn_end" ||
					change?.type === "agent_end"
				) {
					schedule();
				}
			} catch {
				schedule();
			}
		});
		return () => {
			subscription.current = null;
			stopGlobal();
			session.close();
			window.clearTimeout(timer);
			controller?.abort();
		};
	}, [sessionId, enabled]);

	const refresh = useCallback(() => {
		const current = subscription.current;
		if (enabled && current && current.id === sessionId) current.refresh();
	}, [sessionId, enabled]);

	return { payload: enabled && payload?.sessionId === sessionId ? payload : null, refresh };
}

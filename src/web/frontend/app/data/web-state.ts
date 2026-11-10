import { useCallback, useEffect, useRef, useState } from "react";
import type { TreeNode } from "../tree/branch-tree";
import { apiJson } from "./api";
import { type BitLockerSnapshot, newestBitLocker } from "./bitlocker-state";
import {
	type GlobalEventHandler,
	type GlobalEventName,
	resumeEventStreamFromStandby,
	subscribeChannelMode,
	subscribeGlobalEvents,
	subscribeSessionEvents,
} from "./event-stream";
import { OptimisticSessionDeletion } from "./optimistic-session-deletion";
import { recentSessionIds, SessionDetailCache } from "./session-cache";
import { commitMessage, containsAssistant } from "./session-stream";
import { StreamHealth } from "./stream-health";
import { StreamPacer } from "./stream-player";
import type { Message } from "./types";

export type { ContentPart, Message } from "./types";
export type { GlobalEventHandler, GlobalEventName };

export type SessionRow = { id: string; title: string; updatedAt?: string; running: boolean };
/** /api/models 返回的模型条目：reasoning 标记是否支持思考强度。 */
export type Model = { id: string; provider: string; name: string; reasoning: boolean; thinkingLevels?: string[] };
export type QueueItem = {
	id: string;
	text: string;
	status: "pending" | "delivering" | "delivered" | "failed" | "cancelled";
	delivery: "direct" | "queue" | "steer";
	images: { id: string; mimeType: string }[];
	sequence: number;
	uncertain?: boolean;
	error?: string;
};
export type InboxSnapshot = { version: number; paused: boolean; items: QueueItem[] };
export type Question = {
	requestId: string;
	kind: "select" | "confirm" | "input" | "editor";
	title: string;
	options?: string[];
	message?: string;
	prefill?: string;
};
export type SessionDetail = {
	id: string;
	bitlocker?: BitLockerSnapshot;
	messages: Message[];
	entries: { id: string; parentId?: string | null; message: Message }[];
	running: boolean;
	contextUsage?: { tokens: number | null; contextWindow: number; percent: number | null } | null;
	queue: InboxSnapshot;
	ui: Question[];
};

const sessionDetails = new SessionDetailCache((id) =>
	apiJson<SessionDetail>(`/api/sessions/${encodeURIComponent(id)}`),
);

let knownSessions: readonly SessionRow[] = [];

/** 订阅全局事件流：通过聚合流或单流回退分发；enabled 为 false 时不占连接。 */
export function useGlobalEvents(onEvent: GlobalEventHandler, enabled = true) {
	const handler = useRef(onEvent);
	handler.current = onEvent;
	useEffect(() => {
		if (!enabled) return;
		return subscribeGlobalEvents((name, event) => handler.current(name, event));
	}, [enabled]);
}

export function useSessions() {
	const [sessions, setSessions] = useState<SessionRow[]>([]);
	const [stage, setStage] = useState("");
	const [error, setError] = useState("");
	const currentRows = useRef<SessionRow[]>([]);
	const deletions = useRef(new OptimisticSessionDeletion());
	const preloaded = useRef(false);
	const updateRows = useCallback((rows: SessionRow[]) => {
		currentRows.current = rows;
		knownSessions = rows;
		setSessions(rows);
	}, []);
	const requestSequence = useRef(0);
	const refreshTimer = useRef<number | undefined>(undefined);
	const refresh = useCallback(async () => {
		const sequence = ++requestSequence.current;
		try {
			const state = await apiJson<{ sessions: SessionRow[]; stage: string; channelMode?: string }>("/api/state");
			if (sequence !== requestSequence.current) return;
			const nextStage = state.channelMode === "app_active" ? "app_active" : state.stage;
			const rows = deletions.current.reconcile(state.sessions);
			updateRows(rows);
			if (nextStage !== "preview" && nextStage !== "app_active") {
				resumeEventStreamFromStandby();
				sessionDetails.reconcile(state.sessions);
				if (!preloaded.current) {
					preloaded.current = true;
					void sessionDetails.preload(recentSessionIds(rows));
				}
			}
			setStage(nextStage);
			setError("");
		} catch (cause) {
			if (sequence === requestSequence.current)
				setError(cause instanceof Error ? cause.message : "会话列表加载失败");
		}
	}, [updateRows]);
	useEffect(() => {
		void refresh();
		const unsubMode = subscribeChannelMode((mode) => {
			if (mode === "app_active") {
				setStage("app_active");
				setError("");
			}
		});
		return () => {
			unsubMode();
			++requestSequence.current;
			window.clearTimeout(refreshTimer.current);
		};
	}, [refresh]);
	useGlobalEvents(
		(name, event) => {
			if (name === "error") {
				setError("连接已断开。请确认 pi 仍在运行。");
				return;
			}
			if (name === "state") {
				try {
					const payload = JSON.parse(event?.data ?? "null");
					if (payload?.type === "channel_mode_changed") {
						if (payload.mode === "app_active") {
							setStage("app_active");
							setError("");
							return;
						}
						void refresh();
						return;
					}
					if (payload?.type !== "sessions_changed") return;
				} catch {
					return;
				}
			}
			window.clearTimeout(refreshTimer.current);
			refreshTimer.current = window.setTimeout(() => {
				void refresh();
			}, 40);
		},
		!!stage && stage !== "preview" && stage !== "app_active",
	);
	const beginDeletion = useCallback(
		(ids: readonly string[]) => {
			for (const id of ids) sessionDetails.remove(id);
			++requestSequence.current;
			updateRows(deletions.current.begin(currentRows.current, ids));
		},
		[updateRows],
	);
	const settleDeletion = useCallback(
		(deleted: readonly string[], failed: readonly string[]) => {
			++requestSequence.current;
			updateRows(deletions.current.settle(currentRows.current, deleted, failed));
		},
		[updateRows],
	);
	return { sessions, stage, error, refresh, beginDeletion, settleDeletion };
}

export function useSessionTree(id: string | undefined) {
	const activeId = useRef(id);
	activeId.current = id;
	const [tree, setTree] = useState<TreeNode[]>();
	const [leafId, setLeafId] = useState<string | null>(null);
	const [error, setError] = useState("");
	const refresh = useCallback(async () => {
		if (!id) return;
		try {
			const state = await apiJson<{ tree: TreeNode[]; leafId: string | null }>(
				`/api/sessions/${encodeURIComponent(id)}/tree`,
			);
			if (activeId.current !== id) return;
			setTree(state.tree);
			setLeafId(state.leafId);
			setError("");
		} catch (cause) {
			if (activeId.current !== id) return;
			setError(cause instanceof Error ? cause.message : "无法读取分支树");
		}
	}, [id]);
	useEffect(() => {
		setTree(undefined);
		if (!id) return;
		void refresh();
		// message_end、agent_end、session_tree 常同帧到达：尾随去抖合并成一次拉取。
		let refreshTimer: number | undefined;
		const scheduleRefresh = () => {
			window.clearTimeout(refreshTimer);
			refreshTimer = window.setTimeout(() => {
				void refresh();
			}, 80);
		};
		const sub = subscribeSessionEvents(id, (name, data) => {
			if (name === "open") {
				setError("");
				void refresh();
				return;
			}
			if (name === "session") {
				try {
					const change = JSON.parse(data ?? "null");
					if (["message_end", "agent_end", "session_tree"].includes(change?.type)) scheduleRefresh();
				} catch {
					scheduleRefresh();
				}
				return;
			}
			if (name === "reset") {
				void refresh();
				return;
			}
			if (name === "not_found") {
				setError(data || "会话不存在");
				return;
			}
			if (name === "error") {
				setError("事件流已断开。请确认 pi 仍在运行；恢复后页面会自动同步。");
			}
		});
		return () => {
			window.clearTimeout(refreshTimer);
			sub.close();
		};
	}, [id, refresh]);
	return { tree, leafId, error, refresh };
}

/** 会话标题：从会话列表里查当前会话的名字，找不到用「新对话」。 */
export function useSessionTitle(sessionId: string | undefined, initial: string, enabled = true) {
	const [title, setTitle] = useState(() => knownSessions.find((row) => row.id === sessionId)?.title ?? initial);
	const [revision, setRevision] = useState(0);
	useGlobalEvents((name, event) => {
		if (name === "open" || name === "reset") setRevision((value) => value + 1);
		if (name !== "state") return;
		try {
			if (JSON.parse(event?.data ?? "null")?.type === "sessions_changed") setRevision((value) => value + 1);
		} catch {
			/* 下一次会话事件重新同步。 */
		}
	}, enabled && !!sessionId);
	useEffect(() => {
		setTitle(knownSessions.find((row) => row.id === sessionId)?.title ?? initial);
	}, [initial, sessionId]);
	// biome-ignore lint/correctness/useExhaustiveDependencies: sessions_changed 版本号触发标题同步
	useEffect(() => {
		if (!sessionId || !enabled) return;
		let active = true;
		apiJson<{ sessions: { id: string; title: string }[] }>("/api/sessions")
			.then((data) => {
				if (active) setTitle(data.sessions.find((item) => item.id === sessionId)?.title ?? "新对话");
			})
			.catch(() => undefined);
		return () => {
			active = false;
		};
	}, [sessionId, enabled, revision]);
	return title;
}

export function useSessionDetail(id: string | undefined) {
	const activeId = useRef(id);
	const requestSequence = useRef(0);
	activeId.current = id;
	const [detail, setDetail] = useState<SessionDetail | undefined>(() => sessionDetails.peek(id));
	const [streaming, setStreaming] = useState<Message>();
	const bitlockerState = useRef<BitLockerSnapshot>(detail?.bitlocker ?? { revision: -1, requests: [] });
	const [error, setError] = useState("");
	const refresh = useCallback(async () => {
		if (!id) return;
		const sequence = ++requestSequence.current;
		try {
			const state = await sessionDetails.load(id, true);
			if (activeId.current !== id || sequence !== requestSequence.current) return;
			bitlockerState.current = newestBitLocker(bitlockerState.current, state.bitlocker);
			const next = { ...state, bitlocker: bitlockerState.current };
			sessionDetails.put(next);
			setDetail(next);
			setStreaming((message) => (message && !containsAssistant(next, message) ? message : undefined));
			setError("");
		} catch (cause) {
			if (activeId.current !== id || sequence !== requestSequence.current) return;
			setError(cause instanceof Error ? cause.message : "无法读取会话");
		}
	}, [id]);
	useEffect(() => {
		const cached = sessionDetails.peek(id);
		setDetail(cached);
		setStreaming(undefined);
		setError("");
		bitlockerState.current = cached?.bitlocker ?? { revision: -1, requests: [] };
		if (!id) return;
		void refresh();
		const pacer = new StreamPacer(setStreaming);
		// 一次工具循环会连发 agent_start、message_start、message_end 等事件：尾随去抖合并成一次全量拉取。
		let refreshTimer: number | undefined;
		let sub: { close: () => void; reconnect: () => void } | undefined;
		const scheduleRefresh = () => {
			window.clearTimeout(refreshTimer);
			refreshTimer = window.setTimeout(() => {
				void refresh();
			}, 80);
		};
		const health = new StreamHealth(() => {
			void refresh();
			sub?.reconnect();
		});
		sub = subscribeSessionEvents(id, (name, data) => {
			if (name === "open") {
				health.pulse();
				setError("");
				void refresh();
				return;
			}
			if (name === "heartbeat") {
				health.pulse();
				return;
			}
			if (name === "session") {
				health.pulse();
				try {
					const change = JSON.parse(data ?? "null");
					if (change.type === "message_update" && change.message?.role === "assistant")
						pacer.update(change.message);
					if (
						change.type === "message_end" &&
						["assistant", "user", "toolResult"].includes(change.message?.role)
					) {
						if (change.message.role === "assistant") pacer.reset();
						++requestSequence.current;
						setDetail((state) => {
							if (!state) return state;
							const next = commitMessage(state, change.message);
							sessionDetails.put(next);
							return next;
						});
						if (change.message.role === "assistant") setStreaming(undefined);
					}
					if (change.type === "agent_start") {
						setDetail((state) => (state ? { ...state, running: true } : state));
					}
					if (change.type === "bitlocker_requests_changed") {
						bitlockerState.current = newestBitLocker(bitlockerState.current, change);
						setDetail((state) => (state ? { ...state, bitlocker: bitlockerState.current } : state));
					}
					if (change.type === "ui_requests")
						setDetail((state) => (state ? { ...state, ui: change.questions } : state));
					if (
						[
							"message_start",
							"message_end",
							"agent_end",
							"agent_start",
							"session_tree",
							"tool_execution_start",
							"tool_execution_end",
							"tool_execution_update",
						].includes(change.type)
					) {
						if (change.type === "agent_end") pacer.flush();
						scheduleRefresh();
					}
				} catch {
					scheduleRefresh();
				}
				return;
			}
			if (name === "queue") {
				health.pulse();
				try {
					const queue: InboxSnapshot = JSON.parse(data ?? "null");
					setDetail((state) => (state ? { ...state, queue } : state));
				} catch {
					void refresh();
				}
				return;
			}
			if (name === "reset") {
				health.pulse();
				pacer.reset();
				void refresh();
				return;
			}
			if (name === "not_found") {
				setError(data || "会话不存在");
				return;
			}
			if (name === "error") {
				setError("事件流已断开。请确认 pi 仍在运行；恢复后页面会自动同步。");
			}
		});
		return () => {
			++requestSequence.current;
			pacer.reset();
			window.clearTimeout(refreshTimer);
			health.stop();
			sub?.close();
			sub = undefined;
		};
	}, [id, refresh]);
	const currentDetail = detail?.id === id ? detail : sessionDetails.peek(id);
	return { detail: currentDetail, streaming: currentDetail ? streaming : undefined, error, refresh, setDetail };
}

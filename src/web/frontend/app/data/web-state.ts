import { useCallback, useEffect, useRef, useState } from "react";
import type { Message } from "../../../../../agent/home/extensions/web/shared/conversation";
import type { TreeNode } from "../tree/branch-tree";
import { apiJson } from "./api";
import { type BitLockerSnapshot, newestBitLocker } from "./bitlocker-state";
import { OptimisticSessionDeletion } from "./optimistic-session-deletion";
import { recentSessionIds, SessionDetailCache } from "./session-cache";
import { commitMessage, containsAssistant } from "./session-stream";
import { StreamHealth } from "./stream-health";
import { StreamPacer } from "./stream-player";

export type { ContentPart, Message } from "../../../../../agent/home/extensions/web/shared/conversation";

export type SessionRow = { id: string; title: string; updatedAt?: string; running: boolean };
/** /api/models 返回的模型条目：reasoning 标记是否支持思考强度。 */
export type Model = { id: string; provider: string; name: string; reasoning: boolean };
export type QueueItem = {
	id: string;
	text: string;
	status: "pending" | "delivering" | "delivered" | "failed" | "cancelled";
	delivery: "direct" | "queue" | "steer";
	images: { id: string; mimeType: string }[];
	sequence: number;
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

type GlobalEventName = "open" | "state" | "reset" | "error";
type GlobalEventHandler = (name: GlobalEventName, event: MessageEvent | undefined) => void;

// 全局事件流只开一条连接：多个 hook 各自订阅，全部退订后关闭（client-event-listeners）。
const globalHandlers = new Set<GlobalEventHandler>();
let globalSource: EventSource | null = null;
let globalRefs = 0;
function openGlobalEvents(): EventSource {
	if (globalSource) return globalSource;
	const source = new EventSource("/api/events");
	globalSource = source;
	for (const name of ["open", "state", "reset", "error"] as const)
		source.addEventListener(name, (event) => {
			for (const handler of globalHandlers) handler(name, event as MessageEvent);
		});
	return source;
}

/** 订阅全局 /api/events：回调存 ref，身份变化不重连；enabled 为 false 时不占连接。 */
export function useGlobalEvents(onEvent: GlobalEventHandler, enabled = true) {
	const handler = useRef(onEvent);
	handler.current = onEvent;
	useEffect(() => {
		if (!enabled) return;
		openGlobalEvents();
		const listener: GlobalEventHandler = (name, event) => handler.current(name, event);
		globalHandlers.add(listener);
		globalRefs++;
		return () => {
			globalHandlers.delete(listener);
			globalRefs--;
			if (globalRefs === 0 && globalSource) {
				globalSource.close();
				globalSource = null;
			}
		};
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
			const state = await apiJson<{ sessions: SessionRow[]; stage: string }>("/api/state");
			if (sequence !== requestSequence.current) return;
			const rows = deletions.current.reconcile(state.sessions);
			updateRows(rows);
			if (state.stage !== "preview") {
				sessionDetails.reconcile(state.sessions);
				if (!preloaded.current) {
					preloaded.current = true;
					void sessionDetails.preload(recentSessionIds(rows));
				}
			}
			setStage(state.stage);
			setError("");
		} catch (cause) {
			if (sequence === requestSequence.current)
				setError(cause instanceof Error ? cause.message : "会话列表加载失败");
		}
	}, [updateRows]);
	useEffect(() => {
		void refresh();
		return () => {
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
					if (JSON.parse(event?.data ?? "null")?.type !== "sessions_changed") return;
				} catch {
					return;
				}
			}
			window.clearTimeout(refreshTimer.current);
			refreshTimer.current = window.setTimeout(() => {
				void refresh();
			}, 40);
		},
		!!stage && stage !== "preview",
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
		const events = new EventSource(`/api/sessions/${encodeURIComponent(id)}/events`);
		// message_end、agent_end、session_tree 常同帧到达：尾随去抖合并成一次拉取。
		let refreshTimer: number | undefined;
		const scheduleRefresh = () => {
			window.clearTimeout(refreshTimer);
			refreshTimer = window.setTimeout(() => {
				void refresh();
			}, 80);
		};
		events.onopen = () => {
			setError("");
			void refresh();
		};
		events.addEventListener("session", (event) => {
			try {
				const change = JSON.parse((event as MessageEvent).data);
				if (["message_end", "agent_end", "session_tree"].includes(change.type)) scheduleRefresh();
			} catch {
				scheduleRefresh();
			}
		});
		events.addEventListener("reset", () => {
			void refresh();
		});
		events.onerror = () => {
			setError("事件流已断开。请确认 pi 仍在运行；恢复后页面会自动同步。");
		};
		return () => {
			window.clearTimeout(refreshTimer);
			events.close();
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
		let events: EventSource | undefined;
		const scheduleRefresh = () => {
			window.clearTimeout(refreshTimer);
			refreshTimer = window.setTimeout(() => {
				void refresh();
			}, 80);
		};
		const health = new StreamHealth(() => {
			void refresh();
			connect();
		});
		const onSession = (event: Event) => {
			if (event.currentTarget !== events) return;
			health.pulse();
			try {
				const change = JSON.parse((event as MessageEvent).data);
				if (change.type === "message_update" && change.message?.role === "assistant") pacer.update(change.message);
				if (change.type === "message_end" && ["assistant", "user", "toolResult"].includes(change.message?.role)) {
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
		};
		const connect = () => {
			events?.close();
			events = new EventSource(`/api/sessions/${encodeURIComponent(id)}/events`);
			events.onopen = (event) => {
				if (event.currentTarget !== events) return;
				health.pulse();
				setError("");
				void refresh();
			};
			events.addEventListener("heartbeat", (event) => {
				if (event.currentTarget === events) health.pulse();
			});
			events.addEventListener("session", onSession);
			events.addEventListener("queue", (event) => {
				if (event.currentTarget !== events) return;
				health.pulse();
				try {
					const queue: InboxSnapshot = JSON.parse((event as MessageEvent).data);
					setDetail((state) => (state ? { ...state, queue } : state));
				} catch {
					void refresh();
				}
			});
			events.addEventListener("reset", (event) => {
				if (event.currentTarget !== events) return;
				health.pulse();
				pacer.reset();
				void refresh();
			});
			events.onerror = (event) => {
				if (event.currentTarget !== events) return;
				setError("事件流已断开。请确认 pi 仍在运行；恢复后页面会自动同步。");
			};
		};
		connect();
		return () => {
			++requestSequence.current;
			pacer.reset();
			window.clearTimeout(refreshTimer);
			health.stop();
			events?.close();
			events = undefined;
		};
	}, [id, refresh]);
	const currentDetail = detail?.id === id ? detail : sessionDetails.peek(id);
	return { detail: currentDetail, streaming: currentDetail ? streaming : undefined, error, refresh, setDetail };
}

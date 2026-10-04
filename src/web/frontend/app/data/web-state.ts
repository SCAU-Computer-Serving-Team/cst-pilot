import { useCallback, useEffect, useRef, useState } from "react";
import type { TreeNode } from "../tree/branch-tree";
import { apiJson } from "./api";
import { StreamPacer } from "./stream-player";

export type SessionRow = { id: string; title: string; updatedAt?: string; running: boolean };
/** /api/models 返回的模型条目：reasoning 标记是否支持思考强度。 */
export type Model = { id: string; provider: string; name: string; reasoning: boolean };
export type ContentPart =
	| { type: "text"; text: string }
	| { type: "thinking"; thinking: string; redacted?: boolean }
	| { type: "image"; mimeType: string; data: string }
	| { type: "toolCall"; id: string; name: string; arguments: Record<string, unknown> };
export type Message = {
	role: "user" | "assistant" | "toolResult" | "branchSummary";
	content: string | ContentPart[];
	timestamp: number;
	/** 分支总结消息的正文（role 为 branchSummary 时，pi 只给 summary）。 */
	summary?: string;
	fromId?: string;
	toolCallId?: string;
	toolName?: string;
	isError?: boolean;
	stopReason?: string;
	errorMessage?: string;
};
export type QueueItem = {
	id: string;
	text: string;
	status: "pending" | "delivering" | "delivered" | "failed" | "cancelled";
	delivery: "direct" | "queue" | "steer";
	images: { id: string; mimeType: string }[];
	sequence: number;
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
	messages: Message[];
	entries: { id: string; parentId?: string | null; message: Message }[];
	running: boolean;
	contextUsage?: { tokens: number | null; contextWindow: number; percent: number | null } | null;
	queue: InboxSnapshot;
	ui: Question[];
};

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
	const refresh = useCallback(async () => {
		try {
			const state = await apiJson<{ sessions: SessionRow[]; stage: string }>("/api/state");
			setSessions(state.sessions);
			setStage(state.stage);
			setError("");
		} catch (cause) {
			setError(cause instanceof Error ? cause.message : "会话列表加载失败");
		}
	}, []);
	useEffect(() => {
		void refresh();
	}, [refresh]);
	useGlobalEvents(
		(name) => {
			if (name === "error") setError("连接已断开。请确认 pi 仍在运行。");
			else void refresh();
		},
		!!stage && stage !== "preview",
	);
	return { sessions, stage, error, refresh };
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
	const [title, setTitle] = useState(initial);
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
	}, [sessionId, enabled]);
	return title;
}

export function useSessionDetail(id: string | undefined) {
	const activeId = useRef(id);
	const requestSequence = useRef(0);
	activeId.current = id;
	const [detail, setDetail] = useState<SessionDetail>();
	const [streaming, setStreaming] = useState<Message>();
	const [error, setError] = useState("");
	const refresh = useCallback(async () => {
		if (!id) return;
		const sequence = ++requestSequence.current;
		try {
			const state = await apiJson<SessionDetail>(`/api/sessions/${encodeURIComponent(id)}`);
			if (activeId.current !== id || sequence !== requestSequence.current) return;
			setDetail(state);
			setError("");
		} catch (cause) {
			if (activeId.current !== id || sequence !== requestSequence.current) return;
			setError(cause instanceof Error ? cause.message : "无法读取会话");
		}
	}, [id]);
	useEffect(() => {
		setDetail(undefined);
		setStreaming(undefined);
		if (!id) return;
		void refresh();
		const pacer = new StreamPacer(setStreaming);
		// 一次工具循环会连发 agent_start、message_start、message_end 等事件：尾随去抖合并成一次全量拉取。
		let refreshTimer: number | undefined;
		let watchdogTimer: number | undefined;
		let events: EventSource | undefined;
		const scheduleRefresh = () => {
			window.clearTimeout(refreshTimer);
			refreshTimer = window.setTimeout(() => {
				void refresh();
			}, 80);
		};
		// 生成期间的看门狗：活跃轮次里 15 秒收不到任何事件（流式帧高频，不可能静默这么久）
		// 说明连接已成僵尸（服务端已断/被排队），重建 EventSource。
		const armWatchdog = () => {
			window.clearTimeout(watchdogTimer);
			watchdogTimer = window.setTimeout(() => {
				void refresh();
				connect();
			}, 15_000);
		};
		const disarmWatchdog = () => window.clearTimeout(watchdogTimer);
		const onSession = (event: Event) => {
			try {
				const change = JSON.parse((event as MessageEvent).data);
				if (change.type === "message_update" && change.message?.role === "assistant") pacer.update(change.message);
				if (change.type === "ui_requests")
					setDetail((state) => (state ? { ...state, ui: change.questions } : state));
				if (["message_start", "message_end", "agent_end", "agent_start", "session_tree"].includes(change.type)) {
					if (change.type === "message_end" || change.type === "agent_end") {
						pacer.flush();
						setStreaming(undefined);
					}
					scheduleRefresh();
				}
				if (["agent_start", "message_start"].includes(change.type)) armWatchdog();
				else if (change.type === "agent_end") disarmWatchdog();
				else armWatchdog();
			} catch {
				scheduleRefresh();
			}
		};
		const connect = () => {
			events?.close();
			events = new EventSource(`/api/sessions/${encodeURIComponent(id)}/events`);
			events.onopen = () => {
				setError("");
				void refresh();
			};
			events.addEventListener("session", onSession);
			events.addEventListener("queue", (event) => {
				try {
					const queue: InboxSnapshot = JSON.parse((event as MessageEvent).data);
					setDetail((state) => (state ? { ...state, queue } : state));
				} catch {
					void refresh();
				}
			});
			events.addEventListener("reset", () => {
				pacer.reset();
				setStreaming(undefined);
				void refresh();
			});
			events.onerror = () => {
				setError("事件流已断开。请确认 pi 仍在运行；恢复后页面会自动同步。");
			};
		};
		connect();
		return () => {
			pacer.reset();
			window.clearTimeout(refreshTimer);
			window.clearTimeout(watchdogTimer);
			events?.close();
		};
	}, [id, refresh]);
	return { detail, streaming, error, refresh, setDetail };
}
